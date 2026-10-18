// src/codegen/x86_64/elf/init_list.rs
// Global initialization list generation for static variables.

use crate::ast::{find_field, ArraySize, Designator, Expr, InitItem, Type};
use crate::codegen::shared::elf::R_X86_64_PC32;
use crate::codegen::x86_64::emit::expr::aggregate::resolve_designator;
use crate::codegen::x86_64::{Codegen, GlobalVar};
use crate::const_eval;

impl Codegen {
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
                    let needs_elision = inits.iter().any(|item| {
                        matches!(item, InitItem::Expr(e) if !matches!(e, Expr::InitList { .. }))
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
                        let is_float_elem =
                            matches!(&elem_ty.base, Type::Float | Type::Double | Type::LongDouble);
                        let mut k = f;
                        while k <= l {
                            while data.len() < k * elem_size {
                                data.push(0);
                            }
                            let elem_rel = rel.map(|(base, sec)| (base + data.len() as u64, sec));
                            // 复数元素收到复数常量表达式: 按目标分量编码
                            // (eval_const_expr 通用路径对复数只取虚部)
                            let complex_data = match &elem_ty.base {
                                Type::Complex(ci) => Self::complex_init_bytes(value, &ci.base),
                                _ => None,
                            };
                            // 字符串指针元素 (char *arr[] = {"a", "b"}): 不能走
                            // eval_const_expr (字符串会按内容或 0 填充), 单独生成指针槽
                            let elem_data = if let Some(b) = complex_data {
                                b
                            } else if matches!(&elem_ty.base, Type::Ptr(_)) {
                                if let Expr::String { data, .. } =
                                    const_eval::unwrap_string_cast(value)
                                {
                                    self.emit_string_ptr_slot(elem_rel, data, rodata_slots)
                                } else {
                                    self.eval_const_expr(
                                        value,
                                        elem_rel,
                                        elem_size,
                                        is_float_elem,
                                        rodata_slots,
                                    )
                                }
                            } else {
                                self.eval_const_expr(
                                    value,
                                    elem_rel,
                                    elem_size,
                                    is_float_elem,
                                    rodata_slots,
                                )
                            };
                            data.extend_from_slice(&elem_data);
                            k += 1;
                        }
                        continue;
                    }
                    let value = match item {
                        InitItem::Expr(e) => e,
                        InitItem::Designated { designator, value } => {
                            // 链式 designator: resolve_designator 累积偏移
                            match resolve_designator(ty, designator) {
                                Some((off, _, _)) if off == i * elem_size => value,
                                Some((off, _, _)) => {
                                    while data.len() < off {
                                        data.push(0);
                                    }
                                    value
                                },
                                None => continue,
                            }
                        },
                    };
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
                        let nested_ty = if matches!(nested_ty, Type::Int) {
                            &elem_ty.base
                        } else {
                            nested_ty
                        };
                        let elem_rel = rel.map(|(base, sec)| (base + data.len() as u64, sec));
                        if let Some(elem_data) =
                            self.gen_global_init_list(nested_ty, nested, elem_rel, rodata_slots)
                        {
                            data.extend_from_slice(&elem_data);
                        }
                    } else {
                        let is_float_elem =
                            matches!(&elem_ty.base, Type::Float | Type::Double | Type::LongDouble);
                        let elem_rel = rel.map(|(base, sec)| (base + data.len() as u64, sec));
                        // 复数元素收到复数常量表达式: 按目标分量编码
                        // (eval_const_expr 通用路径对复数只取虚部)
                        let complex_data = match &elem_ty.base {
                            Type::Complex(ci) => Self::complex_init_bytes(value, &ci.base),
                            _ => None,
                        };
                        // 字符串指针元素 (char *arr[] = {"a", "b"}): 不能走
                        // eval_const_expr (字符串会按内容或 0 填充), 单独生成指针槽
                        let elem_data = if let Some(b) = complex_data {
                            b
                        } else if matches!(&elem_ty.base, Type::Ptr(_)) {
                            if let Expr::String { data, .. } = const_eval::unwrap_string_cast(value)
                            {
                                self.emit_string_ptr_slot(elem_rel, data, rodata_slots)
                            } else {
                                self.eval_const_expr(
                                    value,
                                    elem_rel,
                                    elem_size,
                                    is_float_elem,
                                    rodata_slots,
                                )
                            }
                        } else {
                            self.eval_const_expr(
                                value,
                                elem_rel,
                                elem_size,
                                is_float_elem,
                                rodata_slots,
                            )
                        };
                        data.extend_from_slice(&elem_data);
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
                            match resolve_designator(ty, designator) {
                                Some((o, s, ft)) => {
                                    // Look up bitfield info for designated field
                                    let bf_info = if let Designator::Field(ref name) = designator {
                                        if let Type::Struct(_, fs, _, _)
                                        | Type::Union(_, fs, _, false) = ty
                                        {
                                            find_field(fs.as_ref(), &name.name).and_then(
                                                |(f, extra)| {
                                                    f.bit_width.map(|bw| {
                                                        (f.bit_offset + (extra as u32) * 8, bw)
                                                    })
                                                },
                                            )
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    };
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
                    let is_float_field =
                        matches!(&field_ty, Some(Type::Float | Type::Double | Type::LongDouble));
                    // 本字段消费的 init item 数 (默认 1)
                    let consumed;
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
                        } => {
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
                                let remaining = match &field_ty {
                                    Some(Type::Array(elem_ty, ArraySize::Const(n))) => {
                                        elem_ty.size() * n - elem_ty.size()
                                    },
                                    _ => field_ty.as_ref().map_or(0, |t| t.size() - sz),
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
                                            is_float_field,
                                            rodata_slots,
                                        )
                                    }
                                } else {
                                    self.eval_const_expr(
                                        value,
                                        field_rel,
                                        sz,
                                        is_float_field,
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
                    let is_float_elem =
                        matches!(&elem_ty.base, Type::Float | Type::Double | Type::LongDouble);
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
                                is_float_elem,
                                rodata_slots,
                            )
                        }
                    } else {
                        self.eval_const_expr(
                            value,
                            elem_rel,
                            elem_size,
                            is_float_elem,
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
                let is_float_inner =
                    matches!(inner.base, Type::Float | Type::Double | Type::LongDouble);

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
                    let real_data =
                        self.eval_const_expr(real_e, rel, inner_size, is_float_inner, rodata_slots);
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
                        is_float_inner,
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
                    let is_float_ty = matches!(ty, Type::Float | Type::Double | Type::LongDouble);
                    if matches!(ty, Type::Ptr(_)) {
                        if let Expr::String { data, .. } = const_eval::unwrap_string_cast(e) {
                            return Some(self.emit_string_ptr_slot(rel, data, rodata_slots));
                        }
                    }
                    data = self.eval_const_expr(e, rel, ty.size(), is_float_ty, rodata_slots);
                }
            },
        }
        if data.is_empty() {
            None
        } else {
            Some(data)
        }
    }

    pub fn eval_global_init(
        &mut self,
        v: &crate::ast::VarDecl,
        sz: usize,
        use_rodata: bool,
    ) -> Option<Vec<u8>> {
        let init = v.init.as_ref()?;
        // 静态地址常量统一拦截 (含 cast 包裹与指针算术基址):
        // globals 中的符号按 data 偏移直算虚地址。
        if let Some((name, addend)) = Self::static_addr_parts(init) {
            if let Some(g) = self.globals_raw.get(&name) {
                let vaddr = self.entry + g.offset + addend as u64;
                return Some(vaddr.to_le_bytes().to_vec());
            }
        }
        let is_float = matches!(v.ty, Type::Float | Type::Double | Type::LongDouble);
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
            Expr::InitList { inits, .. } => {
                let base = if use_rodata {
                    self.rodata.len()
                } else {
                    self.tdata_section.len()
                } as u64;
                self.gen_global_init_list(&v.ty, inits, Some((base, None)), use_rodata)
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
