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

    /// KASAN: 毒化整个栈帧 (shadow bytes 设为 0xF8 = stack mid redzone)
    /// 调用 __asan_poison_memory_region(addr, size)
    /// addr = r13 - frame_size, size = frame_size
    pub fn emit_kasan_stack_poison_all(&mut self) {
        // rdi = r13 - frame_size (帧基址，读解算表)
        let fs = self.frame_layout.frame_size;
        self.emit_lea_r13(7, self.frame_layout.frame_base());
        // mov esi, frame_size
        self.emit_insn(&Insn::raw_bytes(&[0xbe]));
        self.emit_insn(&Insn::raw_bytes(&(fs as i32).to_le_bytes()));
        // call __asan_poison_memory_region (PLT)
        self.emit_call_extern("__asan_poison_memory_region");
    }

    /// KASAN: 解除变量区域的毒化 (shadow bytes 设为 0)
    /// 对 var_map 中每个非 Void 变量调用 __asan_unpoison_memory_region(addr, size)
    pub fn emit_kasan_stack_unpoison_vars(&mut self) {
        // 收集非 Void 变量 (跳过 padding 和红区占位)
        let vars: Vec<(i64, usize)> = self
            .var_map
            .iter()
            .filter(|(name, (_, ty))| {
                !name.starts_with("__pad_") && !name.starts_with("__kasan_rz_") && *ty != Type::Void
            })
            .map(|(_, (off, ty))| (*off, ty.size()))
            .collect();
        for (off, sz) in vars {
            self.emit_lea_r13(7, off);
            // mov esi, size
            self.emit_insn(&Insn::raw_bytes(&[0xbe]));
            self.emit_insn(&Insn::raw_bytes(&(sz as u32).to_le_bytes()));
            // call __asan_unpoison_memory_region
            self.emit_call_extern("__asan_unpoison_memory_region");
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
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8D, 0x3D, 0, 0, 0, 0])); // lea rdi, [rip+disp32]
        self.ubsan_lea_refs.push((lea_pos, data_off));
        // 绝对指针槽: Filename@0, ArrayType@16, IndexType@24 → rodata 目标
        self.ubsan_ptr_refs.push((data_off, 0, file_off));
        self.ubsan_ptr_refs.push((data_off, 16, arr_desc_off));
        self.ubsan_ptr_refs.push((data_off, 24, idx_desc_off));
        lea_pos
    }
}
