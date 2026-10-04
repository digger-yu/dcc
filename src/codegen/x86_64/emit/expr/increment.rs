// src/codegen/x86_64/emit/expr/increment.rs
// Increment/decrement expression code generation (++x, x--, etc.)
//
// 处理前/后自增自减操作的所有类型：整数、浮点、指针、结构体成员访问等。
// 包括位域 read-modify-write 序列、upvar 访问模式、volatile 内存序等。

use crate::ast::find_field;
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;

use crate::diagnostics::Span;

impl super::super::super::Codegen {
    /// 发射增量/减量表达式代码 (pre/post ++/-- for integer, float, pointer, member access)
    pub fn gen_inc_dec(&mut self, e: &Expr, dst: u8, is_pre: bool, is_inc: bool) {
        let inner_ty = e.ty();
        // 浮点 (含 long double): 走 IEEE ±1.0 路径 - 暂不处理，待删除 Phase
        if matches!(inner_ty, Type::Float | Type::Double | Type::LongDouble | Type::Imaginary(_))
            && !inner_ty.is_ptr()
        {
            // TODO: Call gen_float_inc_dec from here during deletion phase
            unimplemented!("float inc/dec - see mod.rs gen_float_inc_dec");
        }
        let step: i64 = if inner_ty.is_ptr() {
            if let Type::Ptr(ref inner) = inner_ty {
                inner.size() as i64
            } else {
                1
            }
        } else {
            1
        };
        // sanitizer 插桩可能把操作数包成 Comma(check, lvalue):
        // 先执行 check 调用，再对真实 lvalue 做读 - 改 - 写，否则
        // 形态匹配失败会静默丢失自增/自减。
        let mut lv: &Expr = e;
        while let Expr::Comma {
            l: check, r: inner, ..
        } = lv
        {
            self.gen_expr(check, 0);
            lv = inner.as_ref();
        }
        if let Expr::Ident { name, .. } = lv {
            // 全局变量：使用 RIP-relative 寻址读 - 改 - 写
            // 必须排除同名局部变量 (C 作用域：局部遮蔽全局)
            //
            // 带段限定的全局（`__seg_gs int gv; gv++`）也必须走本快路径：下面自己写死了
            // `lea` + `mov (%rax)` 字节序列，所以按单一入口 `lvalue_seg_prefix` 取到段后
            // **逐笔贴在真实访存前**（取址那一笔贴不上：LEA 不做内存访问，段前缀对它是
            // 空操作）。`[rsp]` 那两笔读的是栈上的地址副本，不属段内存，不发前缀。
            // 不带段的全局 seg = None ⇒ 一个字节都不多发，产物与改前逐字节相同。
            if self.is_global_not_local(&name.name) {
                let seg = self.lvalue_seg_prefix(lv);
                // 全局变量 inc/dec: 读 - 改 - 写 via RIP-relative
                // 注意：mov eax, [rax] 会零扩展到 rax，销毁 rax 中的地址.
                // 策略：先 push 地址，再 load 值，从栈恢复地址后递增写回.
                // 1. 保存地址到栈
                self.emit_global_addr(0, &name.name); // rax = &c
                self.push(0); // push 地址 → [rsp]
                              // 2. 加载当前值到 rax
                self.emit_global_addr(0, &name.name); // rax = &c
                self.emit_seg(seg);
                match inner_ty.size() {
                    1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00]),
                    2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00]),
                    4 => self.emit(&[0x8B, 0x00]),
                    _ => self.emit(&[0x48, 0x8B, 0x00]),
                }
                // rax = 值 (零扩展), 地址在 [rsp]
                if !is_pre {
                    // 后缀：保存旧值到 rcx (返回用)
                    self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                }
                // 3. 从栈恢复地址到 rax
                self.emit(&[0x48, 0x8B, 0x04, 0x24]); // mov rax, [rsp]
                                                      // rax = 地址，加载值并递增
                self.emit_seg(seg);
                match inner_ty.size() {
                    1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00]),
                    2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00]),
                    4 => self.emit(&[0x8B, 0x00]),
                    _ => self.emit(&[0x48, 0x8B, 0x00]),
                }
                // rax = 值 (零扩展)
                self.emit_inc_dec_reg(0, is_inc, step as i32); // rax = 新值
                                                               // 写回：地址仍在 [rsp], 用 rdx 暂存地址
                self.emit(&[0x48, 0x8B, 0x14, 0x24]); // mov rdx, [rsp]
                self.emit_seg(seg);
                match inner_ty.size() {
                    1 => self.emit(&[0x88, 0x02]),       // mov [rdx], al
                    2 => self.emit(&[0x66, 0x89, 0x02]), // mov [rdx], ax
                    4 => self.emit(&[0x89, 0x02]),       // mov [rdx], eax
                    _ => self.emit(&[0x48, 0x89, 0x02]), // mov [rdx], rax
                }
                // 4. 清理栈并设置返回值
                self.pop(2); // 弹出地址 (丢弃)
                if !is_pre {
                    // 后缀：返回旧值 (在 rcx)
                    self.emit(&[0x48, 0x89, 0xC8]); // mov rax, rcx
                    if dst != 0 && dst != 1 {
                        self.mov(dst, 1);
                    } // dst = rcx
                } else {
                    // 前缀：返回新值 (已在 rax)
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                }
            } else if self.current_func_is_nested && self.upvar_offsets.contains_key(&name.name) {
                // 嵌套函数 upvar: 通过静态链对父帧变量读 - 改 - 写。
                // 不能走 var_offset 懒分配 — 那会在本帧新建同名局部，
                // 副作用丢失 (pr22061-3 形参维度 ++N 就发生在序言)。
                let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                let ty_sz = inner_ty.size().max(1);
                self.load_upvar(dst, parent_off, ty_sz, &name.name);
                if !is_pre {
                    self.push(dst); // 后缀：保存旧值
                }
                self.emit_inc_dec_reg(dst, is_inc, step as i32);
                self.store_upvar(dst, parent_off, ty_sz, &name.name);
                if !is_pre {
                    self.pop(dst); // 后缀：恢复旧值到 dst
                }
            } else if matches!(
                self.home_for(&name.name),
                crate::location::Location::Reg(r) if r.bank == crate::location::RegBank::Gp
            ) {
                // 家在 GP 寄存器上：读-改-写必须在同一个号上做。
                // 下面那个「通用」臂自己拿 `var_offset` + 手写宽度的 load/store，所以绑了
                // 寄存器的变量在这里会只改内存：实测 `for (register long i asm("rbx") = 0;
                // i < n; i++)` 无限循环（-O0/-O1）—— 循环条件读 %rbx 而 `i++` 写 -96(%r13)，
                // 同一个变量两个家（违反设计 §3 的 H4）。走 `mov_from_stack`/`mov_to_stack`
                // 是为了不再长出第三条读家的路，也顺带复用同一个宽度口径（§7.16）。
                self.mov_from_stack(dst, &name.name);
                if !is_pre {
                    self.push(dst); // 后缀：保存旧值
                }
                self.emit_inc_dec_reg(dst, is_inc, step as i32);
                self.mov_to_stack(dst, &name.name);
                if is_pre && inner_ty.size().max(1) < 4 {
                    // 前缀 + 窄有符号：与内存路径同口径，把写回后的值按声明宽度读回来
                    self.mov_from_stack(dst, &name.name);
                }
                if !is_pre {
                    self.pop(dst); // 后缀：恢复旧值到 dst
                }
            } else {
                // 栈槽（或 spill 槽）：坐标走单一读入口 `stack_home_off`，而不是 `var_offset`。
                // 两者对「没有家的名字」完全同值（`home_for` 回退就是 `var_offset`），所以
                // 本行不是行为变更；区别只在于 `Spill` 家现在能被正确翻译（Phase 5.1.c）。
                let off = self.stack_home_off(&name.name);
                let ty_sz = inner_ty.size().max(1);
                // 按类型大小加载 (零扩展小类型到 64 位)
                self.load_stack_sized(dst, off, ty_sz);
                if !is_pre {
                    self.push(dst);
                } // 后缀：保存旧值
                self.emit_inc_dec_reg(dst, is_inc, step as i32);
                // 按类型大小写回栈
                match ty_sz {
                    1 => {
                        let rex = 0x49u8 | if dst >= 8 { 0x04 } else { 0 };
                        self.emit(&[rex, 0x88]);
                        if off.abs() < 128 {
                            self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                        } else {
                            self.emit(&[0x85 | ((dst & 7) << 3)]);
                            self.emit(&(off as i32).to_le_bytes());
                        }
                    },
                    2 => {
                        let rex = 0x49u8 | if dst >= 8 { 0x04 } else { 0 };
                        self.emit(&[0x66, rex, 0x89]);
                        if off.abs() < 128 {
                            self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                        } else {
                            self.emit(&[0x85 | ((dst & 7) << 3)]);
                            self.emit(&(off as i32).to_le_bytes());
                        }
                    },
                    4 => {
                        let rex = 0x49u8 | if dst >= 8 { 0x04 } else { 0 };
                        self.emit(&[rex, 0x89]);
                        if off.abs() < 128 {
                            self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                        } else {
                            self.emit(&[0x85 | ((dst & 7) << 3)]);
                            self.emit(&(off as i32).to_le_bytes());
                        }
                    },
                    _ => self.store_stack(dst, off),
                }
                // 前缀 + sub-int: 存储后寄存器仍保留完整 64 位结果，
                // 需要重新加载截断值以匹配内存中存储的值
                if is_pre && ty_sz < 4 {
                    let signed = Self::is_signed_int(&inner_ty);
                    let rex_b = 0x41u8 | if dst >= 8 { 0x04 } else { 0 };
                    match (ty_sz, signed) {
                        (1, false) => {
                            // movzbl: zero-extend byte
                            self.emit(&[rex_b, 0x0F, 0xB6]);
                        },
                        (1, true) => {
                            // movsbl: sign-extend byte
                            self.emit(&[rex_b, 0x0F, 0xBE]);
                        },
                        (2, false) => {
                            // movzwl: zero-extend word
                            self.emit(&[rex_b, 0x0F, 0xB7]);
                        },
                        (2, true) => {
                            // movswl: sign-extend word
                            self.emit(&[rex_b, 0x0F, 0xBF]);
                        },
                        _ => {},
                    }
                    if off.abs() < 128 {
                        self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                    } else {
                        self.emit(&[0x85 | ((dst & 7) << 3)]);
                        self.emit(&(off as i32).to_le_bytes());
                    }
                }
                if !is_pre {
                    self.pop(dst);
                } // 后缀：恢复旧值到 dst
            }
        } else if matches!(lv, Expr::Deref { .. } | Expr::Index { .. } | Expr::Member { .. }) {
            // Check for bitfield member first
            let bf_info = if let Expr::Member {
                expr,
                field,
                is_arrow,
                ..
            } = lv
            {
                let expr_ty = expr.ty();
                let target_ty = if *is_arrow {
                    if let Type::Ptr(ref p) = expr_ty {
                        p.base.clone()
                    } else {
                        expr_ty
                    }
                } else {
                    expr_ty
                };
                match &target_ty {
                    Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => {
                        find_field(fields, &field.name).and_then(|(f, extra)| {
                            f.bit_width
                                .map(|bw| (bw, f.bit_offset, f.offset + extra, f.ty.size()))
                        })
                    },
                    _ => None,
                }
            } else {
                None
            };
            if let Some((bw, bit_off, field_off, container_size)) = bf_info {
                // Bitfield inc/dec: read-modify-write on container
                self.gen_bitfield_inc_dec(
                    lv,
                    bw,
                    bit_off,
                    field_off,
                    container_size,
                    dst,
                    is_pre,
                    is_inc,
                );
                return;
            }
            // 内存 lvalue: (*p)++ / a[i]-- / s->f++
            let mem_sz = inner_ty.size().max(1);
            // 段前缀：不只看 `*p`，`p[i]++` / `q->a++` / `a[i].f++` 同样要带 ——
            // 基址链下潜取，单一入口见 `lvalue_seg_prefix`。
            let seg_prefix = self.lvalue_seg_prefix(lv);
            if !self.gen_lvalue_addr(lv, 0) {
                return;
            }
            self.push(0); // 保存地址
            if let Some(seg) = seg_prefix {
                self.emit_load_mem_typed_seg(mem_sz, &inner_ty, seg);
            } else {
                self.emit_load_mem_typed(mem_sz, &inner_ty); // rax = 值 (按类型符号/零扩展)
            }
            if !is_pre {
                self.push(0); // 后缀：保存旧值 (栈：[旧值，地址])
            }
            self.emit_inc_dec_reg(0, is_inc, step as i32);
            // 逆序恢复：旧值 (若有) → 地址
            if is_pre {
                self.pop(1); // rcx = 地址
            } else {
                self.pop(2); // rdx = 旧值
                self.pop(1); // rcx = 地址
            }
            if let Some(seg) = seg_prefix {
                self.emit(&[seg]);
            }
            match mem_sz {
                1 => self.emit(&[0x88, 0x01]),       // mov byte [rcx], al
                2 => self.emit(&[0x66, 0x89, 0x01]), // mov word [rcx], ax
                4 => self.emit(&[0x89, 0x01]),       // mov dword [rcx], eax
                _ => self.emit(&[0x48, 0x89, 0x01]), // mov qword [rcx], rax
            }
            if is_pre {
                if dst != 0 {
                    self.mov(dst, 0); // 前缀：dst = 新值
                }
            } else if dst != 2 {
                self.mov(dst, 2); // 后缀：dst = 旧值
            }
        }
    }

    /// 位域增量/减量：读取 - 修改 - 写入容器字。
    fn gen_bitfield_inc_dec(
        &mut self,
        lv: &Expr,
        bw: u32,
        bit_off: u32,
        field_off: usize,
        container_size: usize,
        dst: u8,
        is_pre: bool,
        is_inc: bool,
    ) {
        // Use bf_rmw_depth for frame temp slots (same as bitfield write)
        let depth = self.bf_rmw_depth as i32;
        let addr_slot = (-8 - depth * 24) as i8;
        let old_slot = (-16 - depth * 24) as i8;
        let new_slot = (-24 - depth * 24) as i8;
        self.bf_rmw_depth += 1;

        // 1. Get container address, save to frame slot
        if let Expr::Member { expr, is_arrow, .. } = lv {
            if *is_arrow {
                self.gen_expr(expr, 0);
            } else {
                self.gen_lvalue_addr(expr, 0);
            }
        }
        if field_off != 0 {
            self.emit(&[0x48, 0x05]);
            self.emit(&(field_off as i32).to_le_bytes());
        }
        // rax = container address
        self.emit(&[0x49, 0x89, 0x45, addr_slot as u8]); // [r13+addr_slot] = addr

        // 2. Load container, extract bitfield value
        match container_size {
            4 => self.emit(&[0x8B, 0x00]),
            8 => self.emit(&[0x48, 0x8B, 0x00]),
            2 => {
                self.emit(&[0x66, 0x8B, 0x00]);
                self.emit(&[0x48, 0x0F, 0xB7, 0xC0]);
            },
            1 => self.emit(&[0x48, 0x0F, 0xB6, 0xC0]),
            _ => {},
        }
        if bit_off > 0 {
            self.emit(&[0x48, 0xC1, 0xE8, bit_off as u8]);
        }
        if bw < 64 {
            let mask = (1u64 << bw) - 1;
            if mask <= 0x7FFFFFFF {
                // 掩码可符号扩展到 32 位：and eax, imm32
                self.emit(&[0x48, 0x25]);
                self.emit(&(mask as u32).to_le_bytes());
            } else if mask <= 0xFFFFFFFF {
                // 掩码 fits 32 位零扩展：mov ecx, mask; and rax, rcx
                self.emit(&[0x48, 0xB9]); // mov rcx, imm32 (zero-ext)
                self.emit(&(mask as u32).to_le_bytes());
                self.emit(&[0x48, 0x21, 0xC8]); // and rax, rcx
            } else {
                // 掩码 > 32 位：movabs rcx, mask; and rax, rcx
                self.emit(&[0x48, 0xB9]); // mov rcx, imm64
                self.emit(&mask.to_le_bytes());
                self.emit(&[0x48, 0x21, 0xC8]); // and rax, rcx
            }
        }
        // rax = old bitfield value
        self.emit(&[0x49, 0x89, 0x45, old_slot as u8]); // save old

        // 3. Compute new value
        self.emit_inc_dec_reg(0, is_inc, 1);
        if bw < 64 {
            let mask = (1u64 << bw) - 1;
            if mask <= 0x7FFFFFFF {
                self.emit(&[0x48, 0x25]);
                self.emit(&(mask as u32).to_le_bytes());
            } else if mask <= 0xFFFFFFFF {
                self.emit(&[0x48, 0xB9]);
                self.emit(&(mask as u32).to_le_bytes());
                self.emit(&[0x48, 0x21, 0xC8]);
            } else {
                self.emit(&[0x48, 0xB9]);
                self.emit(&mask.to_le_bytes());
                self.emit(&[0x48, 0x21, 0xC8]);
            }
        }
        // rax = new bitfield value
        self.emit(&[0x49, 0x89, 0x45, new_slot as u8]); // save new

        // 4. Write back: RMW on container
        // Reload address into rcx
        self.emit(&[0x49, 0x8B, 0x4D, addr_slot as u8]); // rcx = addr
                                                         // Load container into rcx
        match container_size {
            4 => self.emit(&[0x8B, 0x09]),
            8 => self.emit(&[0x48, 0x8B, 0x09]),
            2 => {
                self.emit(&[0x66, 0x8B, 0x09]);
                self.emit(&[0x48, 0x0F, 0xB7, 0xC9]);
            },
            1 => self.emit(&[0x48, 0x0F, 0xB6, 0xC9]),
            _ => {},
        }
        // Clear target bits
        let bf_mask = if bw >= 64 { u64::MAX } else { (1u64 << bw) - 1 };
        let inv_mask = !(bf_mask << bit_off);
        if (inv_mask as u32 as i32 as i64 as u64) == inv_mask {
            self.emit(&[0x48, 0x81, 0xE1]);
            self.emit(&(inv_mask as u32).to_le_bytes());
        } else {
            self.emit(&[0x49, 0xB8]);
            self.emit(&inv_mask.to_le_bytes());
            self.emit(&[0x4C, 0x21, 0xC1]);
        }
        // Load new value into rdx, shift, OR
        self.emit(&[0x49, 0x8B, 0x55, new_slot as u8]); // rdx = new val
        if bit_off > 0 {
            self.emit(&[0x48, 0xC1, 0xE2, bit_off as u8]);
        }
        self.emit(&[0x48, 0x09, 0xD1]); // or rcx, rdx
                                        // Store container back
        self.emit(&[0x49, 0x8B, 0x45, addr_slot as u8]); // rax = addr
        match container_size {
            4 => self.emit(&[0x89, 0x08]),
            8 => self.emit(&[0x48, 0x89, 0x08]),
            2 => self.emit(&[0x66, 0x89, 0x08]),
            1 => self.emit(&[0x88, 0x08]),
            _ => {},
        }

        // 5. Set return value
        if is_pre {
            // Return new value
            self.emit(&[0x49, 0x8B, 0x45, new_slot as u8]); // rax = new
            if dst != 0 {
                self.mov(dst, 0);
            }
        } else {
            // Return old value
            self.emit(&[0x49, 0x8B, 0x45, old_slot as u8]); // rax = old
            if dst != 0 {
                self.mov(dst, 0);
            }
        }
        self.bf_rmw_depth -= 1;
    }

    /// 增量/减量单个寄存器 (ADD/SUB reg, immediate)
    fn emit_inc_dec_reg(&mut self, dst: u8, is_inc: bool, step: i32) {
        // ModRM: mod=11, reg=opcode extension (/0=add, /5=sub), r/m=dst register
        let modrm = if is_inc {
            0xC0 | (dst & 7) // /0 (ADD): reg=000, r/m=dst&7
        } else {
            0xE8 | (dst & 7) // /5 (SUB): reg=101, r/m=dst&7
        };
        if dst < 8 {
            self.emit(&[0x48, 0x81, modrm]);
        } else {
            self.emit(&[0x49, 0x81, modrm]);
        }
        self.emit(&step.to_le_bytes());
    }

    /// 浮点增量/减量：暂不实现 - 走 gen_float_inc_dec 路径
    fn gen_float_inc_dec(
        &mut self,
        _lv: &Expr,
        _dst: u8,
        _is_pre: bool,
        _is_inc: bool,
        _ty: &Type,
    ) {
        // Delegate to existing implementation in mod.rs
        // TODO: Move full implementation here when deleting from mod.rs
        unimplemented!("gen_float_inc_dec moved to increment.rs pending deletion phase");
    }

    /// 长双精度增量/减量：x87 FPU 路径 - 占位符
    fn gen_long_double_inc_dec(&mut self, _lv: &Expr, _dst: u8, _is_pre: bool, _is_inc: bool) {
        unimplemented!("long double inc/dec not yet implemented");
    }
}
