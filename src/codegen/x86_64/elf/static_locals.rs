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
    fn register_static_global(&mut self, info: &crate::static_locals::StaticLocalInfo) -> bool {
        let sz = info.size;
        let align = info.alignment;
        let initialized = info.init.is_some();
        // 检查初始化式是否可在编译时求值
        let can_static_init = if initialized {
            match info.init.as_ref().unwrap() {
                Expr::String { .. } => true,
                Expr::InitList { .. } => true, // 尝试, 可能失败
                _ => {
                    let is_float = matches!(info.ty, Type::Float | Type::Double | Type::LongDouble);
                    global_init_bytes(info.init.as_ref().unwrap(), sz, is_float, &info.ty).is_some()
                },
            }
        } else {
            true // 无初始化式 → BSS 零初始化
        };
        let use_data = can_static_init && initialized;
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
        // 尝试写入初始值
        let has_init = if use_data {
            if let Some(init) = &info.init {
                match init {
                    Expr::String { data, .. } => {
                        if matches!(&info.ty, Type::Ptr(_)) {
                            self.data_section.resize(self.data_section.len() + sz, 0);
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
                        let is_float =
                            matches!(info.ty, Type::Float | Type::Double | Type::LongDouble);
                        if let Some(bytes) = global_init_bytes(init, sz, is_float, &info.ty) {
                            self.data_section.extend_from_slice(&bytes);
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
        if !has_init {
            if use_data {
                self.data_section.resize(self.data_section.len() + sz, 0);
            } else if !initialized {
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
