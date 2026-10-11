// src/codegen/x86_64/emit/expr/call.rs
// Expr::Call arm 的 codegen (gen_expr 中的函数调用分支, 含内建函数与
// __builtin_* 分派, sret 缓冲区分配等)。
// 本文件由 mod.rs 逐字搬迁而来 (batch 4d): arm body 原样剪切进 `impl Codegen`
// 新方法 `gen_expr_call`, 语义 diff 为 0。旧影子 `gen_call_expr` 自由函数一并作废。

use crate::ast::{Expr, Type};
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::Codegen;

impl Codegen {
    /// 调用返回值的“落到约定位置”收尾（单一真源，取代三处逐字副本）。
    /// - sret：rax = 缓冲地址；
    /// - 一般标量：结果在 rax，按 dst 搬走；
    /// - x87 st(0) 通道（`long double`，及 -mno-sse 下的全部浮点）：结果**不在**
    ///   rax，而且 dst 常常正持有待用的对象地址，`mov dst, rax` 既无意义又有害。
    fn gen_call_result_move(&mut self, ty: &Type, dst: u8, sret_off: Option<i64>) {
        if let Some(off) = sret_off {
            self.emit_lea_r13(0, off);
            if dst != 0 {
                self.mov(dst, 0);
            }
        } else if dst != 0 && !self.float_in_st0(ty) {
            self.mov(dst, 0);
        }
    }

    /// gen_expr 中 Expr::Call arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_call(&mut self, callee: &Expr, args: &[Expr], ty: &Type, dst: u8) {
        // C 标准语义: (*fp)(...) 中 * 为函数指示符转换 (no-op),
        // 剥掉 Deref 壳后按内部表达式求值, 不真实解引用函数指针
        // (否则会双重加载: 把函数体开头的机器码字节当地址调用).
        //
        // 但**只有结果类型是函数**时那个 Deref 才是函数指示符转换。
        // IR 层的 `Load{addr, ty}` 统一重建为 `Deref{addr, ty}`, 其中
        // `ty` 非函数 (如 C++ 虚派发从 vtable 槽取出的 `void*` 函数指针,
        // 或 C 里 `int (**fp)(int)` 的 `(**fp)()`) 时, Deref 是一次**真读取**,
        // 必须保留: 剥掉它会把槽地址本身当函数入口调用 (直接段错误)。
        let mut call_target: &Expr = callee;
        while let Expr::Deref {
            e: inner,
            ty: deref_ty,
            ..
        } = call_target
        {
            if !matches!(deref_ty, Type::Func(..)) {
                break;
            }
            call_target = inner.as_ref();
        }
        let sret_off = if Self::needs_memory_return(ty) {
            Some(self.alloc_sret_slot(ty.size()))
        } else {
            None
        };
        // 检查是否为直接调用 (callee 是 Ident 且不是局部函数指针变量)
        let direct_name = if let Expr::Ident { name, .. } = call_target {
            if self.var_map.contains_key(&name.name)
                || (self.current_func_is_nested && self.upvar_offsets.contains_key(&name.name))
                || self.is_global_var(&name.name)
            {
                None // 函数指针变量: 走间接调用
            } else {
                Some(name.name.clone())
            }
        } else {
            None
        };
        // gen_builtin 已完整物化 complex 返回的 builtin (conj 系列):
        // result_off 含实/虚部, xmm0=实部 — 下方统一物化须跳过,
        // 否则 movq 打包 store 会覆盖虚部 (20020411-1)
        let mut complex_builtin_done = false;
        if let Some(fname) = direct_name {
            let name_name = fname;
            // __atomic_* / __sync_* 内置函数特殊处理 (生成 lock 前缀指令)
            if (name_name.starts_with("__atomic_") || name_name.starts_with("__sync_"))
                && self.gen_atomic_builtin(&name_name, args, dst)
            {
                // 已成功生成原子指令
            } else if self.gen_builtin(&name_name, args, dst) {
                // 已成功处理 __builtin_* 函数
                if matches!(
                    name_name.as_str(),
                    "__builtin_conj" | "__builtin_conjf" | "__builtin_conjl"
                ) {
                    complex_builtin_done = true;
                }
            } else if Self::is_fortify_builtin(&name_name) {
                // Linux FORTIFY: __builtin___memcpy_chk 等 FORTIFY 宏族展开后
                // 带有 __builtin_ 前缀, 但实际应链接到 libc 的 __*_chk 符号.
                // 剥离前缀后按普通函数调用处理.
                let real_name = &name_name["__builtin_".len()..];
                self.emit_sysv_call(Some(real_name), None, args, sret_off);
                self.gen_call_result_move(ty, dst, sret_off);
            } else if name_name.starts_with("__builtin_") {
                self.gen_unknown_builtin_fallback(&name_name, ty, dst, sret_off);
            } else if matches!(
                name_name.as_str(),
                "creall" | "crealf" | "creal" | "cimagl" | "cimagf" | "cimag"
            ) {
                // complex.h 内建/宏: 实际等价于 __real__/__imag__。
                // glibc 的 <complex.h> 不提供这些名字 (cmathcalls.h 只提供
                // creal/cimag/conj 无 l 后缀版本), GCC 在编译期识别为
                // builtin 直接内联。dcc 之前把它们当普通符号生成 `call creall@plt`,
                // 链接失败; 在此按 builtin 路径 (gen_complex_part) 处理。
                if let Some(arg) = args.first() {
                    let is_real = matches!(name_name.as_str(), "creall" | "crealf" | "creal");
                    self.gen_complex_part(arg, is_real, ty, dst);
                } else if dst != 0 {
                    self.mov_imm(dst, 0);
                }
            } else if matches!(name_name.as_str(), "conjl" | "conjf" | "conj") {
                // complex.h 共轭: 复用 __builtin_conjl/conjf/conj 路径
                // (实部不变, 虚部翻转符号位), 不要再生成对外部符号的调用.
                let builtin = format!("__builtin_{}", name_name);
                if !self.gen_builtin(&builtin, args, dst) {
                    // 兜底: 走普通链接路径 (libc 不存在, 但避免意外崩溃)
                    self.emit_sysv_call(Some(&name_name), None, args, sret_off);
                    self.gen_call_result_move(ty, dst, sret_off);
                } else {
                    // __builtin_conjl 已在内部物化, 标记统一物化跳过
                    complex_builtin_done = true;
                }
            } else if self.error_attr_fns.contains(&name_name) {
                // __attribute__((error("..."))) 函数: 编译期断言失败时
                // 理论上不应到达此处 (常量折叠应消除调用).
                // 作为安全网, 发射 ud2 (unreachable) 避免生成对外部
                // 未定义函数的调用, 防止链接失败.
                self.emit_insn(&Insn::raw_bytes(&[0x0f, 0x0b])); // ud2
            } else {
                self.emit_sysv_call(Some(&name_name), None, args, sret_off);
                self.gen_call_result_move(ty, dst, sret_off);
            }
        } else {
            self.emit_sysv_call(None, Some(call_target), args, sret_off);
            self.gen_call_result_move(ty, dst, sret_off);
        }
        // D10-3（已简化）：`long double` 在 st(0) 返回，而 st(0) 就是本后端
        // `long double` 的 rvalue 值通道，所以调用后**不需要任何搬送**。
        // 旧实现在此处做 `fstp qword (%rsp)` + `movsd (%rsp),%xmm0`：那是为了满
        // 足“ld 内部值通道是 xmm0-f64”的旧不变量，代价是每次调用返回都把真
        // 80-bit 商舍回 53 位尾数（夹具 ld80_arith 的 `cv` 行就是这个错）。
        // 隐藏指针 sret 缓冲那条旧路径已随 `needs_memory_return(LongDouble)==false`
        // 一并消失（见 gen_call_result_move）。
        // vector 返回值: 存入向量临时槽 0
        // <=16B: SSE 类, 被调方在 xmm0 返回, 需存入临时槽
        if matches!(ty, Type::Vector(_, _)) && sret_off.is_none() {
            let toff = self.vector_temp_base(0, ty.size());
            if ty.size() <= 8 {
                // xmm0 → 临时槽 0
                self.store_xmm64_stack(0, toff);
            } else {
                // 16B Vector: xmm0 (全 16B) → 临时槽 0
                self.store_xmm128_stack(0, toff);
            }
        }
        // _Complex 返回值统一物化: 返回端按 SysV ABI 传值
        // (SSE 类: xmm0=实部/xmm1=虚部; 整数类: rax[:rdx];
        //  Memory 类: sret 缓冲), 调用后统一物化到帧临时区
        // result_off, 转换为 dcc complex rvalue 通用约定
        // (内存对 + xmm0=实部 + rax=result_off)。消费端
        // (赋值/spill/传参) 统一按内存对读取, 避免整数 _Complex
        // 返回被按 xmm 读取得垃圾 (complex-6: ctest_int)。
        if let Type::Complex(inner) = ty {
            let result_off = self.complex_result_off();
            if complex_builtin_done {
                // conj 系列已在 gen_builtin 内自物化 (实部
                // result_off, 虚部 result_off+isz, xmm0=实部),
                // 只补 rax=result_off 约定 (下方统一 lea)
            } else if let Some(off) = sret_off {
                // Memory 类 (long double): rax 已指向 sret 缓冲,
                // 拷贝到 result_off 统一内存布局
                self.mov(6, 0); // rsi = rax (缓冲地址)
                self.emit_lea_r13(7, result_off); // rdi = r13+result_off
                self.mov_imm(1, ty.size() as i128);
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // cld; rep movsb
                let _ = off;
            } else if matches!(inner.base, Type::Float) {
                // SysV ABI: _Complex float 是单个 8 字节 SSE 类,
                // 返回端打包在 xmm0 低/高 32 位 (movq 读出即
                // [real@0..4][imag@4..8])。旧实现按 xmm0/xmm1 两
                // 寄存器约定拆分 (那是 _Complex double 的分类),
                // float complex 虚部丢失 (complex-6 test_float)。
                self.emit_xmm0_r13_disp(&[0x66, 0x41, 0x0F, 0xD6], result_off);
                // 实部重载 xmm0 (dcc complex rvalue 约定, movss 4B)
                self.emit_xmm0_r13_disp(&[0xF3, 0x41, 0x0F, 0x10], result_off);
            } else if matches!(inner.base, Type::Double) {
                let pfx = 0xF2u8;
                let isz = inner.base.size() as i64;
                // xmm0=实部 → [result_off]
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                // xmm1=虚部 → xmm0 → [result_off+isz]
                self.emit_insn(&Insn::movsd_xmm(0, 1)); // movsd xmm0, xmm1
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + isz);
                // 实部重载 xmm0 (dcc complex rvalue 约定)
                self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
            } else if matches!(inner.base, Type::LongDouble) {
                // SysV: _Complex long double 经 x87 返回 st0=real、st1=imag,
                // 非 sret。先 fstp tbyte real@result_off（弹出后 imag 升到
                // st0），再 fstp tbyte imag@result_off+16（每分量 16 字节槽）。
                self.emit_x87_fstp_at(13, result_off as i32, &inner.base);
                self.emit_x87_fstp_at(
                    13,
                    (result_off + inner.base.size() as i64) as i32,
                    &inner.base,
                );
            } else {
                // 整数 _Complex: rax(实部/打包) → [result_off],
                // rdx(虚部) → [result_off+8] (>8B)
                self.store_stack(0, result_off);
                if ty.size() > 8 {
                    self.store_stack(2, result_off + 8);
                }
            }
            self.emit_lea_r13(0, result_off);
        }
    }
}
