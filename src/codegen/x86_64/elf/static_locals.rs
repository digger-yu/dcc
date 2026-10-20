// src/codegen/x86_64/elf/static_locals.rs
// Pre-register function-local statics into .data/.bss.

use crate::ast::{Expr, Type};
use crate::codegen::x86_64::mem::global::global_init_bytes;
use crate::codegen::x86_64::{Codegen, GlobalVar};

impl Codegen {
    pub fn pre_register_static_locals(&mut self) {
        // 收集需要注册的 static local 信息 (避免借用冲突)
        let infos: Vec<crate::static_locals::StaticLocalInfo> =
            self.static_local_map.iter_all().cloned().collect();
        for info in &infos {
            let needs_rt = self.register_static_global(info);
            if needs_rt {
                self.static_local_map
                    .mark_runtime_init(&info.owner_func, &info.source_name);
            }
        }
    }
    /// 注册单个 static local 到 data/bss 段.
    /// 返回 true 表示需要运行时初始化.
    /// 递归检测初始化表达式是否含 `&&label` (label address)。
    ///
    /// `&&x` 的 label 只在函数体发射到代码段时才登记进 `self.labels`;
    /// 但 static local 的预注册发生在**函数发射之前** (`pre_register_static_locals`),
    /// 此时 `current_func_name` 为空, 经 `c_label_to_asm` 算出的标签名
    /// `.Ll_x` (缺少函数名前缀) 与后续函数体发射时生成的 `.Ll_<func>_x` 不匹配,
    /// 最终导致 `reloc_sym_index` 报 E1103。
    ///
    /// 含 `&&label` 的初始化式无法在编译期解析 label 地址, 必须走运行时初始化
    /// (函数体发射 label 之后, 由 `emit_global_store` 把 label 地址写回 static 槽)。
    fn init_contains_label_addr(e: &Expr) -> bool {
        match e {
            Expr::LabelAddr { .. } => true,
            Expr::InitList { inits, .. } => inits.iter().any(|it| match it {
                crate::ast::InitItem::Expr(x) => Self::init_contains_label_addr(x),
                crate::ast::InitItem::Designated { value, .. } => {
                    Self::init_contains_label_addr(value)
                },
            }),
            Expr::Cast { e: inner, .. } => Self::init_contains_label_addr(inner),
            Expr::Binop { l, r, .. } => {
                Self::init_contains_label_addr(l) || Self::init_contains_label_addr(r)
            },
            Expr::AddrOf { e: inner, .. } => Self::init_contains_label_addr(inner),
            Expr::Index { arr, idx, .. } => {
                Self::init_contains_label_addr(arr) || Self::init_contains_label_addr(idx)
            },
            Expr::CompoundLit { inits, .. } => Self::init_contains_label_addr_list(inits),
            _ => false,
        }
    }
    fn init_contains_label_addr_list(items: &[crate::ast::InitItem]) -> bool {
        items.iter().any(|it| match it {
            crate::ast::InitItem::Expr(x) => Self::init_contains_label_addr(x),
            crate::ast::InitItem::Designated { value, .. } => Self::init_contains_label_addr(value),
        })
    }

    fn register_static_global(&mut self, info: &crate::static_locals::StaticLocalInfo) -> bool {
        let sz = info.size;
        let align = info.alignment;
        let initialized = info.init.is_some();
        // 含 `&&label` 的初始化式：**属主函数不是嵌套函数**时走编译期静态初始化
        // （data 节 + `pending_data_textlabel_rels`），否则旧行为（运行时初始化）。
        //
        // 为什么值得分这条岔路：computed-goto 解释器（Lua `luaV_execute` 的
        // `disptab`）此前只能运行时初始化 —— 函数**每次被调用**都把整张表重填一遍
        // （60 个 opcode = ~120 条 `lea+mov`）。fib(32) 约 7M 次调用 ≈ 8.4 亿条
        // 指令，实测占 dcc/gcc 性能差距的约四分之一。数据节 + 链接期重定位是 GCC
        // 的做法（`.data.rel.ro`），零运行时代价。
        //
        // 嵌套函数属主一律退回旧路：它的初始化式可能引用**父函数**的标签，而预注册期
        // 没有 `local_labels` 事实，算不准命名空间（算错就是指向别的函数的跳转表）。
        let has_label_ref = info
            .init
            .as_ref()
            .map_or(false, |e| Self::init_contains_label_addr(e));
        let label_owner_ok = has_label_ref
            && !self.nested_func_parent.contains_key(&info.owner_func)
            && !info.owner_func.is_empty();
        let use_data = if label_owner_ok {
            true // 由下面的初始化发射成败决定（失败则回退 BSS + 运行时初始化）
        } else if initialized && !has_label_ref {
            match info.init.as_ref().unwrap() {
                Expr::String { .. } => true,
                Expr::InitList { .. } => true, // 尝试, 可能失败
                _ => {
                    let is_float = info.ty.is_real_float();
                    global_init_bytes(info.init.as_ref().unwrap(), sz, is_float, &info.ty).is_some()
                },
            }
        } else {
            false // 无初始化式, 或含 &&label 但不能静态化 → 不进静态初始化
        };
        if use_data {
            self.data_align = self.data_align.max(align as u64);
        } else {
            self.bss_align = self.bss_align.max(align as u64);
        }
        let offset = if use_data {
            let off = (self.data_section.len() + align - 1) & !(align - 1);
            while self.data_section.len() < off {
                self.data_section.push(0);
            }
            self.data_section.len() as u64
        } else {
            self.bss_size = self.bss_size.div_ceil(align as u64) * align as u64;
            self.bss_size
        };
        // 标签命名空间：含 &&label 的静态初始化在预注册期求值，属主函数名必须可见
        // （`c_label_to_asm` 在 `current_func_name` 为空时读它）。
        let prev_owner = self.static_local_label_owner.take();
        if label_owner_ok {
            self.static_local_label_owner = Some(info.owner_func.clone());
        }
        // 尝试写入初始值
        let has_init = if use_data {
            if let Some(init) = &info.init {
                match init {
                    Expr::String { data, .. } => {
                        if matches!(&info.ty, Type::Ptr(_)) {
                            // 指针类型 (static char *s = "..."): 字符串进 rodata 池,
                            // data 节指针槽通过 .rodata section 符号 + R_X86_64_64
                            // 重定位指向字符串 (链接时写入 rodata 基址 + 偏移)。
                            // 与文件作用域全局同一路径 (codegen.rs 的
                            // `char *s = "hello"` 分支) —— 旧实现只 resize 出 sz 个
                            // **零字节**且不发重定位, 于是函数局部 static 指针恒为
                            // NULL (各级别均复现): 实测 sqlite shell.c zAutoColumn 的
                            // zTabFill 在 `.import` 自动列名路径上传给
                            // sqlite3_prepare_v2 的 zSql 是 NULL ⇒ SQLITE_MISUSE ⇒
                            // rc_err_oom_die 断言中止。
                            let data = data.clone();
                            if !self.string_pool.contains_key(&data) {
                                let o = self.rodata.len() as u64;
                                self.rodata.extend_from_slice(&data);
                                self.string_pool.insert(data.clone(), o);
                            }
                            let str_off = *self.string_pool.get(&data).unwrap();
                            let ref_off = self.data_section.len() as u64;
                            self.data_section.resize(ref_off as usize + sz, 0);
                            self.pending_data_rodata_rels.push((ref_off, str_off, None));
                        } else if matches!(&info.ty, Type::Array(_, _)) {
                            let copy_len = data.len().min(sz);
                            self.data_section.extend_from_slice(&data[..copy_len]);
                            for _ in copy_len..sz {
                                self.data_section.push(0);
                            }
                        } else {
                            self.data_section.extend_from_slice(data);
                            for _ in data.len()..sz {
                                self.data_section.push(0);
                            }
                        }
                        true
                    },
                    Expr::InitList { inits, .. } => {
                        let base = self.data_section.len() as u64;
                        if let Some(init_data) =
                            self.gen_global_init_list(&info.ty, inits, Some((base, None)), false)
                        {
                            self.data_section.extend_from_slice(&init_data);
                            for _ in init_data.len()..sz {
                                self.data_section.push(0);
                            }
                            true
                        } else {
                            false
                        }
                    },
                    _ => {
                        let is_float = info.ty.is_real_float();
                        if let Some(bytes) = global_init_bytes(init, sz, is_float, &info.ty) {
                            self.data_section.extend_from_slice(&bytes);
                            true
                        } else if let Expr::LabelAddr { .. } = init {
                            // 标量 `static void *p = &&lab;`：走 eval_const_expr 的
                            // LabelAddr 分支（登记 pending_data_textlabel_rels）。
                            let base = self.data_section.len() as u64;
                            let bytes = self.eval_const_expr(
                                init,
                                Some((base, None)),
                                sz,
                                &info.ty,
                                false,
                            );
                            self.data_section.extend_from_slice(&bytes);
                            for _ in bytes.len()..sz {
                                self.data_section.push(0);
                            }
                            true
                        } else {
                            false
                        }
                    },
                }
            } else {
                false
            }
        } else {
            false
        };
        self.static_local_label_owner = prev_owner;
        if !has_init {
            if use_data {
                self.data_section.resize(self.data_section.len() + sz, 0);
            } else {
                // 走 BSS 路径的两种情形:
                //  1. 无初始化式 (zero-init) → bss_size 已按 align 分配 offset
                //  2. 含 `&&label` 等运行时初始化式 → bss_size 同样已分配 offset,
                //     需要把 BSS 大小推进到下一个变量
                self.bss_size += sz as u64;
            }
        }
        let section = if has_init { 3 } else { 4 };
        // 如果 data 段分配了空间但初始化失败, 回退到 BSS
        if use_data && !has_init {
            let new_len = offset as usize;
            self.data_section.truncate(new_len);
            self.bss_align = self.bss_align.max(align as u64);
            self.bss_size = self.bss_size.div_ceil(align as u64) * align as u64;
            let bss_off = self.bss_size;
            self.bss_size += sz as u64;
            self.globals_raw.insert(
                info.mangled_name.clone(),
                GlobalVar {
                    offset: bss_off,
                    size: sz,
                    ty: info.ty.clone(),
                    init: None,
                    section: 4,
                    is_tls: false,
                    in_data_section: true,
                },
            );
        } else {
            self.globals_raw.insert(
                info.mangled_name.clone(),
                GlobalVar {
                    offset,
                    size: sz,
                    ty: info.ty.clone(),
                    init: None,
                    section,
                    is_tls: false,
                    in_data_section: true,
                },
            );
        }
        // 返回是否需要运行时初始化
        initialized && !has_init
    }
}
