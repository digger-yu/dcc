// src/codegen/x86_64/elf/init_list.rs
// Global initialization list generation for static variables.

use crate::ast::{ArraySize, Designator, Expr, InitItem, Type};
use crate::codegen::shared::elf::R_X86_64_PC32;
use crate::codegen::x86_64::emit::expr::aggregate::{designator_bitfield, resolve_designator};
use crate::codegen::x86_64::{Codegen, GlobalVar};
use crate::const_eval;

impl Codegen {
    /// 数组**单个元素**的初始化器 → 该元素的字节编码（单一真源）。
    ///
    /// 两个调用者必须走同一条路：普通元素项，和 GNU 范围设计符
    /// `[f ... l] = v` 展开出的每个 k。范围分支曾经自己内联了一份「只认标量」
    /// 的编码，于是 `[0 ... 1023] = { .first = NULLS_MARKER(0) }`（内核
    /// `kernel/ucount.c` 的 `ucounts_hashtable`）被 `eval_const_expr` 当成
    /// 不支持的表达式写成了全 0：空链表的 NULLS 哨兵（`1`）退化成 NULL，
    /// `hlist_nulls_for_each_entry_rcu` 把 NULL 当节点，`iter->uid` 读 +0x18
    /// 当场 #PF（报告 §5.37）。
    fn encode_array_elem(
        &mut self,
        elem_ty: &Type,
        value: &Expr,
        elem_rel: Option<(u64, Option<usize>)>,
        elem_size: usize,
        rodata_slots: bool,
    ) -> Option<Vec<u8>> {
        // 带花括号的聚合元素（`{ .first = 1 }` / `(T){…}`）必须递归生成；
        // 判据与数组分支顶部的 is_braced_init 同源，否则复合字面量元素会被
        // 当成标量平铺（§5.34 的同一条口径）。
        //
        // 例外：**指针元素**收到聚合型字面量是 C11 6.3.2.1p3 的退化取地址
        // （`ptr_decay_literal`），必须落到 `eval_const_expr` 的物化+重定位路径；
        // 当成嵌套列表会把内容铺进指针槽（kbuild23 的 `CMDS()` 表）。
        if let Expr::InitList {
            inits: nested,
            ty: nested_ty,
            ..
        }
        | Expr::CompoundLit {
            ty: nested_ty,
            inits: nested,
            ..
        } = value
        {
            if !Self::ptr_decay_literal(elem_ty, value) {
                let nty = if matches!(nested_ty, Type::Int) {
                    elem_ty.clone()
                } else {
                    nested_ty.clone()
                };
                let mut bytes = self.gen_global_init_list(&nty, nested, elem_rel, rodata_slots)?;
                bytes.resize(elem_size, 0);
                return Some(bytes);
            }
        }
        // C11 6.7.9p14: 字符串字面量初始化 char 数组元素（二维数组的一行，
        // 如 char w[][6] = {"hello","world"}）——整个字符串填入该元素，
        // 尾部补零到 elem_size。不能走 eval_const_expr，否则字符串会被
        // 当标量只取首字节（实测将两行错写为 "hw"）。
        if Self::is_char_array(elem_ty) {
            if let Expr::String { data, .. } = value {
                let mut row = vec![0u8; elem_size];
                let n = data.len().min(elem_size);
                row[..n].copy_from_slice(&data[..n]);
                return Some(row);
            }
        }
        // 复数元素收到复数常量表达式: 按目标分量编码
        // (eval_const_expr 通用路径对复数只取虚部)
        if let Type::Complex(ci) = elem_ty {
            if let Some(b) = Self::complex_init_bytes(value, &ci.base) {
                return Some(b);
            }
        }
        // 字符串指针元素 (char *arr[] = {"a", "b"}): 不能走
        // eval_const_expr (字符串会按内容或 0 填充), 单独生成指针槽
        if matches!(elem_ty, Type::Ptr(_)) {
            if let Expr::String { data, .. } = const_eval::unwrap_string_cast(value) {
                return Some(self.emit_string_ptr_slot(elem_rel, data, rodata_slots));
            }
        }
        Some(self.eval_const_expr(value, elem_rel, elem_size, elem_ty, rodata_slots))
    }

    pub fn gen_global_init_list(
        &mut self,
        ty: &Type,
        inits: &[InitItem],
        rel: Option<(u64, Option<usize>)>,
        rodata_slots: bool,
    ) -> Option<Vec<u8>> {
        let mut data = Vec::new();
        match ty {
            Type::Array(elem_ty, _) => {
                let elem_size = elem_ty.size();
                // C11 6.7.9p14 brace elision: 元素类型本身是聚合体 (嵌套数组/结构/
                // 联合) 但初始值未用花括号包裹时, 把扁平标量分组到子聚合中, 例如
                // int a[8][6] = {14 个标量} → 每行 6 个分一组。否则每个标量会被
                // 当成一整行 (elem_size 字节) 错铺 (pr58626.c 全局数组全零)。
                // 与局部初始化路径 gen_init_list_at 的 group_for_brace_elision 保持一致。
                let effective_inits;
                if matches!(
                    &elem_ty.base,
                    Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
                ) {
                    // 判断是否需要 brace elision: 存在「扁平标量」时才分组。但一个
                    // `Expr::String` 直接初始化 char 数组元素时是**整行**初始化
                    // (C11 6.7.9p14: "abc" 等价 {'a','b','c','\0'}), 不是标量——若把它
                    // 计入触发条件, `char w[][6] = {"hello","world"}` 会被按 sub_count=6
                    // 把两个字符串挤进第 0 行, 再各按标量只写首字节 (得 "hw")。
                    // 同理 `(T){…}` 复合字面量也自带花括号 (判据单一真源 is_braced_init);
                    // 旧判据只认 `Expr::InitList`, 内核 `static_calls_table` 的
                    // `(struct lsm_static_call){…}` 元素因此被平铺写坏 (§5.34)。
                    let needs_elision = inits.iter().any(|item| {
                        matches!(item, InitItem::Expr(e)
                            if !Self::is_braced_init(e)
                                && !(Self::is_char_array(&elem_ty.base)
                                    && matches!(e, Expr::String { .. })))
                    });
                    effective_inits = if needs_elision {
                        Self::group_for_brace_elision(&elem_ty.base, inits)
                    } else {
                        inits.to_vec()
                    };
                } else {
                    effective_inits = inits.to_vec();
                }
                let inits = &effective_inits;
                // GNU 范围设计符的**链式**形式 `[f ... l].field = v` 等价于对
                // k∈[f,l] 依次写 `[k].field = v`。顶层 `[f ... l] = v` 由下面的
                // Range 早退分支处理；链式形式此前交给 resolve_designator，而
                // Range 在那里固定返回 None ⇒ 整项被静默跳过，目标槽位停在 0。
                // 展开成显式 Index 项后，它们走通用的指定符路径（含位域、链式
                // 子成员、乱序回写），不再需要第二套编码（报告 §5.37）。
                let expanded;
                let inits = if inits.iter().any(|it| {
                    matches!(
                        it,
                        InitItem::Designated {
                            designator: Designator::Chain(ch),
                            ..
                        } if matches!(ch.first(), Some(Designator::Range(_, _)))
                    )
                }) {
                    let mut out: Vec<InitItem> = Vec::with_capacity(inits.len());
                    for it in inits {
                        let InitItem::Designated {
                            designator: Designator::Chain(ch),
                            value,
                        } = it
                        else {
                            out.push(it.clone());
                            continue;
                        };
                        let Some(Designator::Range(first, last)) = ch.first() else {
                            out.push(it.clone());
                            continue;
                        };
                        let (Ok(f), Ok(l)) = (const_eval::eval(first), const_eval::eval(last)) else {
                            out.push(it.clone());
                            continue;
                        };
                        let (f, l) = (f.max(0) as usize, l.max(0) as usize);
                        if l < f {
                            continue;
                        }
                        for k in f..=l {
                            let mut sub = ch.clone();
                            sub[0] = Designator::Index(Expr::Int {
                                v: k as i128,
                                span: crate::diagnostics::Span::point("<init>", 0, 0),
                                ty: Type::Int,
                            });
                            out.push(InitItem::Designated {
                                designator: Designator::Chain(sub),
                                value: value.clone(),
                            });
                        }
                    }
                    expanded = out;
                    &expanded[..]
                } else {
                    inits
                };
                for (i, item) in inits.iter().enumerate() {
                    // GNU 范围设计符 [first ... last]: 范围内每个元素写同一值
                    if let InitItem::Designated {
                        designator: Designator::Range(first, last),
                        value,
                    } = item
                    {
                        let (Ok(f), Ok(l)) = (const_eval::eval(first), const_eval::eval(last))
                        else {
                            continue;
                        };
                        let (f, l) = (f as usize, l as usize);
                        if l < f {
                            continue;
                        }
                        let mut k = f;
                        while k <= l {
                            while data.len() < k * elem_size {
                                data.push(0);
                            }
                            let elem_rel = rel.map(|(base, sec)| (base + data.len() as u64, sec));
                            // 元素值编码走与普通元素同一个入口：范围设计符只是
                            // 「同一个值写到多个下标」，不该有自己的一套编码
                            // （§5.37：内联标量编码把聚合值写成全 0）。
                            let Some(elem_data) = self.encode_array_elem(
                                &elem_ty.base,
                                value,
                                elem_rel,
                                elem_size,
                                rodata_slots,
                            ) else {
                                break;
                            };
                            data.extend_from_slice(&elem_data);
                            // 编码短于元素宽度时补齐，否则下一个 k 的起点错位
                            if data.len() < (k + 1) * elem_size {
                                data.resize((k + 1) * elem_size, 0);
                            }
                            k += 1;
                        }
                        continue;
                    }
                    // 目标字节偏移: 仅 designated 项有; 元素字节按它写 (而非无脑
                    // append), 以支持乱序/前跳后又回退的设计符 (pr69714:
                    // ...[2],[12],[3],[4],[5] —— 旧代码遇 off<data.len() 时会把
                    // 值追加到末尾, 指定槽位停留在 0)。
                    let mut target_off: Option<usize> = None;
                    let value = match item {
                        InitItem::Expr(e) => e,
                        InitItem::Designated { designator, value } => {
                            // 链式 designator: resolve_designator 累积偏移
                            match resolve_designator(ty, designator) {
                                Some((off, _, _)) => {
                                    target_off = Some(off);
                                    value
                                },
                                None => continue,
                            }
                        },
                    };
                    let write_base = target_off.unwrap_or(data.len());
                    let elem_rel = rel.map(|(base, sec)| (base + write_base as u64, sec));
                    // C11 6.7.9p14：一维字符数组的花括号字符串初始化
                    // （`char a[] = { "hello" };`）等价于逐个字符铺开 —— 字符串
                    // 从当前位置起**横跨连续元素**。`encode_array_elem` 只在元素本身
                    // 是字符数组（二维数组的一行）时按整行处理；标量元素走到那里会
                    // 退化成「只取首字节」（内核 fs/coredump.c 的 `core_modes[]`
                    // 因此只剩首字符，sysctl 报 `No maxlen`）。
                    if target_off.is_none() {
                        if let (Expr::String { data: bytes, .. }, true) = (
                            value,
                            matches!(
                                &elem_ty.base,
                                Type::Char
                                    | Type::SChar
                                    | Type::UChar
                                    | Type::Char8
                                    | Type::Char16
                                    | Type::Char32
                                    | Type::Short
                                    | Type::UShort
                                    | Type::Int
                                    | Type::UInt
                            ),
                        ) {
                            while data.len() < write_base {
                                data.push(0);
                            }
                            let n = bytes.len();
                            if data.len() < write_base + n {
                                data.resize(write_base + n, 0);
                            }
                            data[write_base..write_base + n].copy_from_slice(bytes);
                            continue;
                        }
                    }
                    let elem_data_opt = self.encode_array_elem(
                        &elem_ty.base,
                        value,
                        elem_rel,
                        elem_size,
                        rodata_slots,
                    );
                    if let Some(elem_data) = elem_data_opt {
                        match target_off.take() {
                            Some(off) => {
                                // 乱序/回退设计符: 按偏移覆写 (越界则先补零).
                                if data.len() < off + elem_data.len() {
                                    data.resize(off + elem_data.len(), 0);
                                }
                                data[off..off + elem_data.len()].copy_from_slice(&elem_data);
                            },
                            None => data.extend_from_slice(&elem_data),
                        }
                    }
                    while data.len() < (i + 1) * elem_size {
                        data.push(0);
                    }
                }
            },
            Type::Struct(_name, fields, _, _) | Type::Union(_name, fields, _, false) => {
                let total_size = ty.size();
                data.resize(total_size, 0);
                let mut field_idx = 0;
                let mut init_idx = 0;
                while init_idx < inits.len() {
                    let item = &inits[init_idx];
                    let (off, sz, value, field_ty, field_info) = match item {
                        InitItem::Expr(e) => {
                            // 跳过匿名位域 ( unnamed bitfield padding )
                            while field_idx < fields.len()
                                && fields[field_idx].name.is_empty()
                                && fields[field_idx].bit_width.is_some()
                            {
                                field_idx += 1;
                            }
                            if field_idx < fields.len() {
                                let f = &fields[field_idx];
                                let info = f.bit_width.map(|bw| (f.bit_offset, bw));
                                field_idx += 1;
                                (f.offset, f.ty.size(), e, Some(f.ty.clone()), info)
                            } else {
                                break; // 多余初始值, 忽略
                            }
                        },
                        InitItem::Designated { designator, value } => {
                            // C11 6.7.9p17: 设计符之后的**位置**初始化项从「刚描述的子对象
                            // 的下一个」继续，而不是从结构体首字段重新开始。旧实现只更新了
                            // 目标偏移、没推进 field_idx，于是
                            //   `{ .a=1, .b=2, .c=3, .d=4, 5, 6, (unsigned long)&bd }`
                            // 后面的位置项全部写回 a/b/c（且按字段宽度截断：&bd 被当成
                            // 4 字节发 R_X86_64_32）。8250_exar.c 的 CTI_EXAR_DEVICE 正是
                            // 这个形状，最终链接报 R_X86_64_32 截断。
                            // 链式设计符（`.a.b = …`）的 next subobject 在同一父对象内，
                            // 顶层 field_idx 不能简单 +1，这里保持旧行为不动。
                            if let Designator::Field(name) = designator {
                                if let Type::Struct(_, fs, _, _) | Type::Union(_, fs, _, false) = ty {
                                    if let Some(idx) =
                                        fs.iter().position(|f| f.name.as_str() == name.name)
                                    {
                                        field_idx = idx + 1;
                                    }
                                }
                            }
                            match resolve_designator(ty, designator) {
                                Some((o, s, ft)) => {
                                    // 位域信息：链式设计符（`.bits.p = 1`）必须沿链
                                    // 下潜到最终字段，否则退化成普通标量写 —— 值落在
                                    // 字段起始字节、丢掉单元内移位（报告 §5.25：内核
                                    // `def_idts[]` 的 IDT 门因此 p=0，首个 ud2 即 triple fault）。
                                    let bf_info = designator_bitfield(ty, designator);
                                    (o, s, value, Some(ft), bf_info)
                                },
                                None => {
                                    init_idx += 1;
                                    continue;
                                },
                            }
                        },
                    };
                    let field_rel = rel.map(|(base, sec)| (base + off as u64, sec));
                    // 本字段消费的 init item 数 (默认 1)
                    let consumed;
                    // 指针字段收到聚合型字面量（`{ .commands = (const u8 []){…} }`）
                    // 是 C11 6.3.2.1p3 的退化取地址，不是「把内容填进指针槽」：
                    // 必须落到 `eval_const_expr` 的物化+重定位路径。
                    let decays_to_ptr = field_ty
                        .as_ref()
                        .is_some_and(|ft| Self::ptr_decay_literal(ft, value));
                    // 嵌套初始化列表 ({ { [0 ... N] = v } }): 递归生成字段数据,
                    // 否则 eval_const_expr 会把整个 InitList 当成不支持表达式置零
                    let field_data = match value {
                        Expr::InitList {
                            inits: nested,
                            ty: nested_ty,
                            ..
                        }
                        | Expr::CompoundLit {
                            ty: nested_ty,
                            inits: nested,
                            ..
                        } if !decays_to_ptr => {
                            consumed = 1;
                            let nty = if matches!(nested_ty, Type::Int) {
                                field_ty.clone().unwrap_or_else(|| ty.clone())
                            } else {
                                nested_ty.clone()
                            };
                            self.gen_global_init_list(&nty, nested, field_rel, rodata_slots)
                                .unwrap_or_else(|| vec![0; sz])
                        },
                        _ => {
                            // 聚合字段 (数组/struct) 收到标量值: 收集后续标量 init items
                            // 填充聚合元素 (C 允许无括号扁平初始化, 如
                            // struct { char c; int a[2]; } = {'X', 8, 9})
                            // 例外: 字符串初始化聚合字段 (char c[8] = "val") 只消费
                            // 一个 item, 字符串填满数组 (剩余补零), 后续标量属于下一
                            // 字段 (C 标准 6.7.9p14: "abc" 等价于 {'a','b','c','\0'})
                            let is_aggregate = matches!(
                                &field_ty,
                                Some(
                                    Type::Array(..)
                                        | Type::Struct(..)
                                        | Type::Union(..)
                                        | Type::Vector(..)
                                )
                            );
                            if is_aggregate
                                && matches!(item, InitItem::Expr(_))
                                && !matches!(value, Expr::String { .. })
                            {
                                let mut sub_inits: Vec<InitItem> =
                                    vec![InitItem::Expr(value.clone())];
                                let mut k = init_idx + 1;
                                // 计算该聚合字段剩余字节数 (第一个标量已占一个元素大小)
                                //
                                // 两处必须是 `saturating_sub`：`remaining` 的语义是「本字段还剩多少
                                // 字节」，负值在 C 里不存在；无符号直减在 debug profile 下当场
                                // panic（实测 `gcc.c-torture/execute/20030305-1.c` 在 -O0 就碰上了
                                // `n == 0` 的数组字段），在 release 下则回绕成一个天文数字，把
                                // 后面的初始化元素全部吃进本字段（静默误编译）。
                                let remaining = match &field_ty {
                                    Some(Type::Array(elem_ty, ArraySize::Const(n))) => (elem_ty
                                        .size()
                                        .saturating_mul(*n))
                                        .saturating_sub(elem_ty.size()),
                                    _ => field_ty
                                        .as_ref()
                                        .map_or(0, |t| t.size().saturating_sub(sz)),
                                };
                                // 对于数组: 每个标量占 elem_size 字节
                                if let Some(Type::Array(elem_ty, _)) = &field_ty {
                                    let es = elem_ty.size();
                                    let max_extra = remaining.checked_div(es).unwrap_or(0);
                                    let mut extra = 0usize;
                                    while extra < max_extra && k < inits.len() {
                                        if let InitItem::Expr(e) = &inits[k] {
                                            sub_inits.push(InitItem::Expr(e.clone()));
                                            extra += 1;
                                            k += 1;
                                        } else {
                                            break;
                                        }
                                    }
                                } else {
                                    // struct/union 嵌套: 只消费 1 个
                                }
                                consumed = k - init_idx;
                                let sub_ty = field_ty.clone().unwrap();
                                self.gen_global_init_list(
                                    &sub_ty,
                                    &sub_inits,
                                    field_rel,
                                    rodata_slots,
                                )
                                .unwrap_or_else(|| vec![0; sz])
                            } else {
                                consumed = 1;
                                // 复数字段收到复数常量表达式: 按目标分量编码
                                // (eval_const_expr 通用路径对复数只取虚部)
                                let complex_data = match &field_ty {
                                    Some(Type::Complex(ci)) => {
                                        Self::complex_init_bytes(value, &ci.base)
                                    },
                                    _ => None,
                                };
                                if let Some(b) = complex_data {
                                    b
                                } else if matches!(&field_ty, Some(Type::Ptr(_))) {
                                    if let Expr::String { data, .. } =
                                        const_eval::unwrap_string_cast(value)
                                    {
                                        self.emit_string_ptr_slot(field_rel, data, rodata_slots)
                                    } else {
                                        self.eval_const_expr(
                                            value,
                                            field_rel,
                                            sz,
                                            field_ty.as_ref().unwrap_or(&Type::Int),
                                            rodata_slots,
                                        )
                                    }
                                } else {
                                    self.eval_const_expr(
                                        value,
                                        field_rel,
                                        sz,
                                        field_ty.as_ref().unwrap_or(&Type::Int),
                                        rodata_slots,
                                    )
                                }
                            }
                        },
                    };
                    init_idx += consumed;
                    // 位域字段: 将值打包到正确的位位置
                    if let Some((bit_off, bit_w)) = field_info {
                        if bit_w <= 64 {
                            // 将 field_data 解释为整数
                            let mut val: u64 = 0;
                            for (j, &b) in field_data.iter().enumerate() {
                                if j < 8 {
                                    val |= (b as u64) << (j * 8);
                                }
                            }
                            // 掩码到 bit_w 位
                            let mask = if bit_w >= 64 { u64::MAX } else { (1u64 << bit_w) - 1 };
                            val &= mask;
                            // 写入 data: off 是存储单元字节偏移, bit_off 是单元内位偏移
                            let byte_off = off + (bit_off / 8) as usize;
                            let bit_shift = bit_off % 8;
                            let shifted = val << bit_shift;
                            let bytes = shifted.to_le_bytes();
                            let write_len = (bit_shift + bit_w).div_ceil(8) as usize;
                            for (j, &b) in bytes[..write_len.min(8)].iter().enumerate() {
                                if byte_off + j < data.len() {
                                    data[byte_off + j] |= b;
                                }
                            }
                            continue;
                        }
                    }
                    for (j, &b) in field_data.iter().enumerate() {
                        // 柔性数组字段: 扩展 data 缓冲区以容纳数据
                        if off + j >= data.len() {
                            data.resize(off + j + 1, 0);
                        }
                        data[off + j] = b;
                    }
                }
            },
            Type::Vector(elem_ty, n) => {
                let elem_size = elem_ty.size();
                data.resize(elem_size * n, 0);
                for (i, item) in inits.iter().enumerate() {
                    if i >= *n {
                        break;
                    }
                    let value = match item {
                        InitItem::Expr(e) => e,
                        _ => continue,
                    };
                    let elem_rel = rel.map(|(base, sec)| (base + (i * elem_size) as u64, sec));
                    // 复数元素收到复数常量表达式: 按目标分量编码
                    // (eval_const_expr 通用路径对复数只取虚部)
                    let complex_data = match &elem_ty.base {
                        Type::Complex(ci) => Self::complex_init_bytes(value, &ci.base),
                        _ => None,
                    };
                    // 字符串指针元素 (向量元素为指针时): 生成指针槽
                    let elem_data = if let Some(b) = complex_data {
                        b
                    } else if matches!(&elem_ty.base, Type::Ptr(_)) {
                        if let Expr::String { data, .. } = const_eval::unwrap_string_cast(value) {
                            self.emit_string_ptr_slot(elem_rel, data, rodata_slots)
                        } else {
                            self.eval_const_expr(
                                value,
                                elem_rel,
                                elem_size,
                                &elem_ty.base,
                                rodata_slots,
                            )
                        }
                    } else {
                        self.eval_const_expr(
                            value,
                            elem_rel,
                            elem_size,
                            &elem_ty.base,
                            rodata_slots,
                        )
                    };
                    let off = i * elem_size;
                    for (j, &b) in elem_data.iter().enumerate() {
                        if off + j < data.len() {
                            data[off + j] = b;
                        }
                    }
                }
            },
            Type::Complex(ref inner) => {
                // 复数类型初始化: 分别处理实部和虚部
                let inner_size = inner.size();
                let total_size = inner_size * 2;
                data.resize(total_size, 0);
                // 分量目标类型 = 复数的内型（eval_const_expr 按它选字节编码）
                let complex_inner_ty: &Type = &inner.base;

                // 收集实部和虚部的初始化表达式
                let mut real_expr = None;
                let mut imag_expr = None;

                for (i, item) in inits.iter().enumerate() {
                    if let InitItem::Expr(e) = item {
                        if i == 0 {
                            real_expr = Some(e);
                        } else if i == 1 {
                            imag_expr = Some(e);
                            break;
                        }
                    }
                }

                // 单个复数常量表达式 ({1+2i} / {2i}): 拆解实虚部按目标
                // 分量编码。不能当实部标量求值 (eval_full 对复数只返回
                // 虚部, 会把整个复数当实部写入)
                if imag_expr.is_none() {
                    if let Some(e0) = real_expr {
                        if matches!(e0.ty(), Type::Complex(_) | Type::Imaginary(_)) {
                            if let Some(bytes) = Self::complex_init_bytes(e0, &inner.base) {
                                if bytes.len() <= total_size {
                                    data[..bytes.len()].copy_from_slice(&bytes);
                                    return Some(data);
                                }
                            }
                        }
                    }
                }

                // 处理实部
                if let Some(real_e) = real_expr {
                    let real_data = self.eval_const_expr(
                        real_e,
                        rel,
                        inner_size,
                        complex_inner_ty,
                        rodata_slots,
                    );
                    for (j, &b) in real_data.iter().enumerate() {
                        if j < inner_size {
                            data[j] = b;
                        }
                    }
                }

                // 处理虚部
                if let Some(imag_e) = imag_expr {
                    let imag_rel = rel.map(|(base, sec)| (base + inner_size as u64, sec));
                    let imag_data = self.eval_const_expr(
                        imag_e,
                        imag_rel,
                        inner_size,
                        complex_inner_ty,
                        rodata_slots,
                    );
                    for (j, &b) in imag_data.iter().enumerate() {
                        if inner_size + j < total_size {
                            data[inner_size + j] = b;
                        }
                    }
                }
            },
            _ => {
                if let Some(InitItem::Expr(e)) = inits.first() {
                    if matches!(ty, Type::Ptr(_)) {
                        if let Expr::String { data, .. } = const_eval::unwrap_string_cast(e) {
                            return Some(self.emit_string_ptr_slot(rel, data, rodata_slots));
                        }
                    }
                    data = self.eval_const_expr(e, rel, ty.size(), ty, rodata_slots);
                }
            },
        }
        if data.is_empty() {
            None
        } else {
            Some(data)
        }
    }

    /// 静态初始化式的「花括号列表」视图 —— 单一判据点。
    ///
    /// `= { … }` 与 `= (T){ … }` 是同一个初始化语义 (C11 6.5.2.5: 复合字面量的
    /// 初始值列表就是花括号列表, 其对象表示与直接声明同形), 包裹列表的**聚合类型**
    /// 转换也不改变对象表示。仓库里所有 AST 遍历 (`src/ir.rs`、`src/ast.rs`、
    /// `src/ast_cmp_fold.rs` …) 早就把两种写法并成一条臂; x86_64 的**静态初始化**
    /// 站点却只认 `Expr::InitList`, 于是
    ///
    /// ```c
    /// struct s g = (struct s){ .a = 7 };   // 落到标量常量兜底 → 求不出字节 → 静默零初始化
    /// ```
    ///
    /// 内核全树都在用这条写法: `#define __SPIN_LOCK_UNLOCKED(x) (spinlock_t)
    /// __SPIN_LOCK_INITIALIZER(x)`, 而 `early_pfn_lock` 的 boot 缺陷正是它的
    /// 函数局部 static 版本 (报告 §5.14)。
    /// 标量目标的转换 (`(int){1}`) 语义上仍是标量, **不**算列表。
    pub fn static_brace_list(init: &Expr) -> Option<&[InitItem]> {
        match init {
            Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => Some(inits),
            Expr::Cast { ty, e, .. }
                if matches!(
                    *ty,
                    Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_, _)
                ) =>
            {
                Self::static_brace_list(e)
            },
            _ => None,
        }
    }

    pub fn eval_global_init(
        &mut self,
        v: &crate::ast::VarDecl,
        sz: usize,
        use_rodata: bool,
    ) -> Option<Vec<u8>> {
        let init = v.init.as_ref()?;
        // 指针目标的聚合型字面量是**地址**（C11 6.3.2.1p3 退化，判据见
        // `ptr_decay_literal`）。必须放在 `static_brace_list` 之前：那个判据
        // 只看「有没有花括号」，不看目标类型。
        if Self::ptr_decay_literal(&v.ty, init) {
            let base = if use_rodata {
                self.rodata.len()
            } else {
                self.tdata_section.len()
            } as u64;
            if let Some(bytes) =
                self.ptr_decay_init(&v.ty, init, Some((base, None)), sz, use_rodata)
            {
                return Some(bytes);
            }
        }
        // 花括号列表 (含复合字面量与包裹它的聚合类型转换) 共用同一份字节生成,
        // 写法判据单点在 [`Self::static_brace_list`]; 对象类型取**声明类型**,
        // 与下面 `gen_global_init_list(&v.ty, …)` 的旧 `Expr::InitList` 臂一致。
        if let Some(inits) = Self::static_brace_list(init) {
            let base = if use_rodata {
                self.rodata.len()
            } else {
                self.tdata_section.len()
            } as u64;
            return self.gen_global_init_list(&v.ty, inits, Some((base, None)), use_rodata);
        }
        // 静态地址常量统一拦截 (含 cast 包裹与指针算术基址):
        // globals 中的符号按 data 偏移直算虚地址。
        if let Some((name, addend)) = Self::static_addr_parts(init) {
            if let Some(g) = self.globals_raw.get(&name) {
                let vaddr = self.entry + g.offset + addend as u64;
                return Some(vaddr.to_le_bytes().to_vec());
            }
        }
        let is_float = v.ty.is_real_float();
        match init {
            Expr::String { data, .. } => {
                // 指针类型 (char *s = "hello"): 字符串字面量应存字符串地址,
                // 而非字符串内容 (13 字节 > 8 字节指针槽, 会污染布局)。
                // ET_EXEC 路径在 compile_exec 中单独处理 (rodata 池 + resolve
                // 修补绝对地址); 此处返回 None 让调用方零初始化兜底。
                if matches!(&v.ty, Type::Ptr(_)) {
                    None
                } else {
                    Some(data.clone())
                }
            },
            Expr::AddrOf { e, .. } => {
                // 1. 首先尝试命名全局变量 + 偏移 (如 &a[3], &s.field)
                if let Some((base_name, byte_off)) =
                    Self::eval_global_lvalue_offset(e, &self.globals_raw)
                {
                    if let Some(g) = self.globals_raw.get(&base_name) {
                        let vaddr = self.entry + g.offset + byte_off as u64;
                        return Some(vaddr.to_le_bytes().to_vec());
                    } else {
                        return Some(vec![0u8; 8]);
                    }
                }
                // 2. Compound literal 取地址: 发射为匿名静态对象
                // (&(struct B){...} 是合法的 C99 文件地址常量)
                if let Some((anon_name, vaddr)) = self.materialize_compound_literal(e, use_rodata) {
                    // 记录匿名对象到 globals_raw (供后续嵌套 AddrOf 查找)
                    let sz = e.ty().size().max(1);
                    let rodata_offset = vaddr - self.entry;
                    self.globals_raw.insert(
                        anon_name.clone(),
                        GlobalVar {
                            offset: rodata_offset,
                            size: sz,
                            ty: e.ty().clone(),
                            init: None,
                            section: 1,
                            is_tls: false,
                            in_data_section: false,
                        },
                    );
                    return Some(vaddr.to_le_bytes().to_vec());
                }
                None
            },
            // 整数/浮点常量: 统一走 eval_full, 浮点目标转位模式
            _ => Self::global_init_bytes(init, sz, is_float, &v.ty),
        }
    }
}
