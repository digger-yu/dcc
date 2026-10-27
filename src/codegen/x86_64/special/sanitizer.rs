// src/codegen/x86_64/special/sanitizer.rs
// UBSan（-fbounds-check）诊断数据与 KASAN 栈红区插桩的 x86_64 codegen。
//
// 本文件由 mod.rs 逐字搬迁而来 (batch D)：`ubsan_type_name` / `ubsan_type_info`
// / `emit_kasan_stack_poison_all` / `emit_kasan_stack_unpoison_vars`
// / `emit_ubsan_oob_data` 原样剪切进 `impl Codegen` 块，函数体一字未改，
// 仅把 `fn` 提为 `pub fn`（mod.rs 是祖先模块，需要可见性）。
// 夹在中间的 `emit_call_extern` 是通用外部调用发射器（被全模块广泛复用，
// 不属 sanitizer 语义），留在 mod.rs 未搬。
//
// 原影子 `emit_sanitize_bounds_check`（unimplemented!() 占位，与生产
// `emit_ubsan_oob_data` + libubsan 运行时协议不一致）已按"以生产代码为准"作废删除。

use crate::ast::*;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::Codegen;

impl Codegen {
    // 指令发射
    /// UBSAN 诊断用 C 类型名 (仅用于打印, 不追求与编译器完全一致)。
    pub fn ubsan_type_name(ty: &Type) -> String {
        match ty {
            Type::Void => "void".into(),
            Type::Bool => "_Bool".into(),
            Type::Char => "char".into(),
            Type::SChar => "signed char".into(),
            Type::UChar => "unsigned char".into(),
            Type::Short => "short".into(),
            Type::UShort => "unsigned short".into(),
            Type::Int => "int".into(),
            Type::UInt => "unsigned int".into(),
            Type::Long => "long".into(),
            Type::ULong => "unsigned long".into(),
            Type::LongLong => "long long".into(),
            Type::ULongLong => "unsigned long long".into(),
            Type::Int128 => "__int128".into(),
            Type::UInt128 => "unsigned __int128".into(),
            Type::Float => "float".into(),
            Type::Double => "double".into(),
            Type::LongDouble => "long double".into(),
            Type::Ptr(inner) => format!("{} *", Self::ubsan_type_name(&inner.base)),
            Type::Complex(inner) => format!("_Complex {}", Self::ubsan_type_name(&inner.base)),
            _ => "<unknown>".into(),
        }
    }

    /// libubsan TypeDescriptor 的 TypeKind/TypeInfo 编码 (与 gcc libsanitizer
    /// ubsan.cc 一致): 整型 TypeKind=0, TypeInfo=(log2(bit_width)<<1)|signed;
    /// 浮点 TypeKind=1, TypeInfo=bit_width; 其他 (数组/结构) TypeKind=0xffff,
    /// TypeInfo=0 (handler 仅用 TypeName 打印)。
    pub fn ubsan_type_info(ty: &Type) -> (u16, u16) {
        match ty {
            Type::Bool => (0, 1),
            Type::Char | Type::SChar => (0, (3 << 1) | 1),
            Type::UChar => (0, 3 << 1),
            Type::Short => (0, (4 << 1) | 1),
            Type::UShort => (0, 4 << 1),
            Type::Int => (0, (5 << 1) | 1),
            Type::UInt => (0, 5 << 1),
            Type::Long => (0, (6 << 1) | 1),
            Type::ULong => (0, 6 << 1),
            Type::LongLong => (0, (6 << 1) | 1),
            Type::ULongLong => (0, 6 << 1),
            Type::Int128 => (0, (7 << 1) | 1),
            Type::UInt128 => (0, 7 << 1),
            Type::Float => (1, 32),
            Type::Double => (1, 64),
            Type::LongDouble => (1, 80),
            _ => (0xffff, 0),
        }
    }

    // ===== KASAN 栈红区插桩 =====

    /// KASAN/ASan 栈插桩：进函数时先把整个帧的 shadow 清零，再**只毒真实红区**。
    ///
    /// 为什么不是旧那样「毒整个帧 → 只解除 `var_map` 里具名变量的毒」：那条模型下
    /// 一大片活内存终身带毒 —— 发射期才登记的晚分配带（`<agg_member_tmp>` /
    /// `__compound_lit_*` / `__vmsize_*` / ap 槽，序言跑时它们还不在 `var_map` 里）、
    /// spill 带、以及按 `ty.size()` 只解了一半的对象尾部。把这类槽的地址交给未插桩的
    /// 库（Tcl / libc）正常写入，就报 `use-after-poison` 假阳性（实测：SQLite
    /// `oserror.test` 在 ASan 构建里报的写者是 libtcl，而地址在当时的 sp 之上，
    /// 即落在活帧里）。退出路径也不加 unpoison 调用 —— 那会踩坏 rax/rdx/xmm0
    /// 里的返回值；「进则先清」已经覆盖了陈旧毒的回收。
    ///
    /// 红区条目由 `collect_frame_var_decls` 一侧建在 `var_map` 里（`__kasan_rz_*`，每个
    /// 8 字节，紧跟在对应变量下方）。这里只毒那些**确实没被任何活对象占据**的条目；
    /// 按（偏移, 名字）排序后发射，否则 HashMap 迭代序会泄进产物（不可复现）。
    ///
    /// 为什么不直接信红区条目自己的算术：那个算术是「本变量起始地址往下 8 字节 × j」，
    /// 而排布推进是另一条路径（带同名去重、对齐补齐、跳 padding 等多条分支），两边并非
    /// 总是同步 —— 实测一个只有 `struct S s; char b[8];` 的 `main` 里，红区条目正好落在
    /// `b` 的槽上，于是 libc 的 `memcpy` 正常写入被报 `use-after-poison`（同一源码
    /// gcc 的 ASan 构建干净）。红区在几何上就是「未被占用的帧字节」，所以把判据
    /// 定在几何事实上：与任何活区间相交的红区条目一律不毒。
    pub fn emit_kasan_stack_poison_redzones(&mut self) {
        // 活区间：`var_map` 里除红区条目以外的每一个对象（按槽数取整，保守地当作全占）
        let mut live: Vec<(i64, i64)> = self
            .var_map
            .iter()
            .filter(|(name, _)| !name.starts_with("__kasan_rz_"))
            .map(|(_, (off, ty))| (*off, off + Self::var_alloc_slots(ty).saturating_mul(8)))
            .collect();
        live.sort_unstable();
        // 红区候选（排序后发射，保证产物可复现）
        let mut rzs: Vec<(i64, String)> = self
            .var_map
            .iter()
            .filter(|(name, (_, ty))| name.starts_with("__kasan_rz_") && *ty == Type::Void)
            .map(|(name, (off, _))| (*off, name.clone()))
            .collect();
        rzs.sort();
        self.shield_arg_regs_begin();
        // 1) 先清整帧（包括上一位住户留下的陈旧毒）
        let fs = self.frame_layout.frame_size;
        self.emit_lea_r13(7, self.frame_layout.frame_base());
        self.emit_insn(&Insn::raw_bytes(&[0xbe]));
        self.emit_insn(&Insn::raw_bytes(&(fs as u32).to_le_bytes()));
        self.emit_call_extern("__asan_unpoison_memory_region");
        // 2) 再只毒那些真的是空隙的红区
        for (off, _) in rzs {
            let end = off + 8;
            if live.iter().any(|&(ls, le)| ls < end && le > off) {
                continue; // 这个条目与某个活对象重叠 ⇒ 不是红区，毒它就是假阳性
            }
            self.emit_lea_r13(7, off);
            self.emit_insn(&Insn::raw_bytes(&[0xbe]));
            self.emit_insn(&Insn::raw_bytes(&8u32.to_le_bytes()));
            self.emit_call_extern("__asan_poison_memory_region");
        }
        self.shield_arg_regs_end();
    }

    /// 序言插桩用的**入参寄存器屏蔽**：开始处把 SysV 的六个整数参数号与 xmm0-7 存好，
    /// 结束处原样恢复。
    ///
    /// 为什么必须有：这段插桩发在序言里，而参数落栈发生在**函数体开头**，比这里晚；
    /// `__asan_poison_memory_region` / `__asan_unpoison_memory_region` 是真调用，被调方对
    /// 这些号没有保留义务 —— 不屏蔽就是把参数当垃圾用（实测：ASan 下
    /// `sqlite3_snprintf` 看到的指针参数成了 0x1，第一个 `%s` 就 SEGV）。
    /// 量上配平：6 个 push = 48 字节、xmm 区 128 字节，都是 16 的倍数 ⇒ 调用点的
    /// rsp 对齐与改前一致；r13 相对寻址也不受影响（本帧基址存在 r13 里）。
    fn shield_arg_regs_begin(&mut self) {
        for reg in [7u8, 6, 2, 1, 8, 9] {
            self.push(reg);
        }
        self.emit_insn(&Insn::raw_with_gas(
            &[0x48, 0x81, 0xEC, 0x80, 0x00, 0x00, 0x00],
            "sub $0x80, %rsp",
        ));
        for i in 0..8u8 {
            self.emit_insn(&Insn::raw_with_gas(
                &[0x0F, 0x11, 0x44 + (i << 3), 0x24, (i as i8 * 16) as u8],
                &format!("movaps %xmm{}, {}(%rsp)", i, i * 16),
            ));
        }
    }

    /// 与 `shield_arg_regs_begin` 严格反向配平（先恢复 xmm，再 pop 整数号）。
    fn shield_arg_regs_end(&mut self) {
        for i in (0..8u8).rev() {
            self.emit_insn(&Insn::raw_with_gas(
                &[0x0F, 0x10, 0x44 + (i << 3), 0x24, (i as i8 * 16) as u8],
                &format!("movaps {}(%rsp), %xmm{}", i * 16, i),
            ));
        }
        self.emit_insn(&Insn::raw_with_gas(
            &[0x48, 0x81, 0xC4, 0x80, 0x00, 0x00, 0x00],
            "add $0x80, %rsp",
        ));
        for reg in [9u8, 8, 1, 2, 6, 7] {
            self.pop(reg);
        }
    }

    /// 为 -fbounds-check 生成 libubsan OutOfBoundsData (gcc 布局):
    ///   SourceLocation { const char *Filename; u32 Line; u32 Column; }
    ///   const TypeDescriptor &ArrayType;  // offset 16, 打印类型名用
    ///   const TypeDescriptor &IndexType;  // offset 24, 解码越界索引值用
    /// TypeDescriptor / 文件名字符串进 .rodata (handler 不写, 只读安全);
    /// OutOfBoundsData 的模式差异: ET_REL 的 .rodata 只读, 而 libubsan
    /// SourceLocation::acquire() 对 Column 做原子写 (atomic_exchange) →
    /// 必须放 .data, compile_rel 生成 PC32/R_X86_64_64 重定位; ET_EXEC 镜像
    /// 在单个 RWX 段 (elf.rs flags=7), rodata 运行时可写, resolve() 直接修补。
    /// 返回 LEA 指令的 code_pos (修补 disp32 用)。
    pub fn emit_ubsan_oob_data(
        &mut self,
        idx_ty: &Type,
        elem_ty: &Type,
        arr_type_name: &str,
        span: &crate::diagnostics::Span,
    ) -> u64 {
        // TypeDescriptor { u16 TypeKind; u16 TypeInfo; char TypeName[]; }
        // TypeName 带引号: "'int [3]'", "'int'"
        let mk_desc = |ty: &Type, name: &str| -> Vec<u8> {
            let (kind, info) = Self::ubsan_type_info(ty);
            let mut d = Vec::new();
            d.extend_from_slice(&kind.to_le_bytes());
            d.extend_from_slice(&info.to_le_bytes());
            d.push(b'\'');
            d.extend_from_slice(name.as_bytes());
            d.push(b'\'');
            d.push(0);
            while !d.len().is_multiple_of(8) {
                d.push(0);
            }
            d
        };
        // 索引类型描述符 (IndexType): 解码越界索引值 (符号/位宽)
        let idx_desc_off = self.rodata.len() as u64;
        self.rodata
            .extend_from_slice(&mk_desc(idx_ty, &Self::ubsan_type_name(idx_ty)));
        // 数组类型描述符 (ArrayType): 数组非标量 → TK_Unknown, 仅打印
        let arr_desc_off = self.rodata.len() as u64;
        self.rodata
            .extend_from_slice(&mk_desc(elem_ty, arr_type_name));
        // 文件名字符串 → string_pool (跨调用去重)
        let file_bytes = span.file.as_bytes().to_vec();
        if !self.string_pool.contains_key(&file_bytes) {
            let o = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&file_bytes);
            self.rodata.push(0);
            self.string_pool.insert(file_bytes.clone(), o);
        }
        let file_off = self.string_pool[&file_bytes];
        // OutOfBoundsData (32 字节, 8 对齐): 指针槽先写 0, 由
        // resolve() (ET_EXEC) / compile_rel (ET_REL) 修补。
        let data_off = if self.relocatable {
            self.data_align = self.data_align.max(8);
            let off = (self.data_section.len() + 7) & !7;
            while self.data_section.len() < off {
                self.data_section.push(0);
            }
            self.data_section.len() as u64
        } else {
            let off = (self.rodata.len() + 7) & !7;
            while self.rodata.len() < off {
                self.rodata.push(0);
            }
            self.rodata.len() as u64
        };
        let mut data_bytes = Vec::with_capacity(32);
        data_bytes.extend_from_slice(&0u64.to_le_bytes()); // Filename 占位
        data_bytes.extend_from_slice(&span.line.to_le_bytes());
        data_bytes.extend_from_slice(&span.col.to_le_bytes());
        data_bytes.extend_from_slice(&0u64.to_le_bytes()); // &ArrayType 占位
        data_bytes.extend_from_slice(&0u64.to_le_bytes()); // &IndexType 占位
        if self.relocatable {
            self.data_section.extend_from_slice(&data_bytes);
        } else {
            self.rodata.extend_from_slice(&data_bytes);
        }
        // 生成 LEA rdi, [rip+disp32] 占位
        let lea_pos = self.code.len() as u64;
        self.emit_insn(&Insn::lea_rip(7)); // lea rdi, [rip+disp32]
        self.ubsan_lea_refs.push((lea_pos, data_off));
        // 绝对指针槽: Filename@0, ArrayType@16, IndexType@24 → rodata 目标
        self.ubsan_ptr_refs.push((data_off, 0, file_off));
        self.ubsan_ptr_refs.push((data_off, 16, arr_desc_off));
        self.ubsan_ptr_refs.push((data_off, 24, idx_desc_off));
        lea_pos
    }
}
