// src/codegen/x86_64/mem/object_resolver.rs
//
// `ObjectResolver` trait 实现：lvalue 地址计算的后端无关层（crate::semantics）
// 通过它查询标识符的存储类别。原先寄居在迁移期的 stubs.rs 里，stubs.rs 作废后
// 按职责归位到 mem/。
#![allow(unused_imports)]

use crate::codegen::x86_64::Codegen;

// ObjectResolver trait 实现（lvalue 地址计算的后端无关层需要）。
// 语义与 monolith (28a9063) 的 resolve_identity_semantic /
// resolve_storage_semantic 一致：查找优先级 var_map > vla_offsets >
// upvar_offsets(嵌套) > globals（经 resolve_global_key，含 static local）。
// 旧存根直查 globals_raw[name] 导致块级 static 数组被误分类为
// StackFrame{0}，元素访问从 r13+0 读垃圾（st2 类段错误）。
impl crate::semantics::ObjectResolver for Codegen {
    fn resolve_identity(&self, name: &str) -> crate::semantics::ObjectIdentity {
        use crate::semantics::ObjectIdentity;
        if self.var_map.contains_key(name) {
            return ObjectIdentity::Local;
        }
        if self.vla_offsets.contains_key(name) {
            return ObjectIdentity::Vla;
        }
        if self.current_func_is_nested && self.upvar_offsets.contains_key(name) {
            return ObjectIdentity::Upvar;
        }
        let key = self.resolve_global_key(name);
        if self.globals_raw.contains_key(key) {
            return ObjectIdentity::Global {
                resolved_key: key.to_string(),
            };
        }
        // 兜底: 懒分配路径 (与 var_offset 行为一致)
        ObjectIdentity::Local
    }

    fn resolve_storage(
        &self,
        id: &crate::semantics::ObjectIdentity,
        name: &str,
    ) -> crate::semantics::StorageInfo {
        use crate::semantics::{ObjectIdentity, StorageClass, StorageInfo, StorageType};
        match id {
            ObjectIdentity::Local => {
                let (offset, ty) = self
                    .var_map
                    .get(name)
                    .cloned()
                    .unwrap_or((0, crate::ast::Type::Int));
                StorageInfo {
                    class: StorageClass::Stack,
                    obj_type: crate::semantics::storage_type_from_ast(&ty),
                    symbol_key: None,
                    stack_offset: Some(offset),
                    is_tls: false,
                }
            },
            ObjectIdentity::Vla => {
                let offset = self.vla_offsets.get(name).copied().unwrap_or(0);
                StorageInfo {
                    class: StorageClass::Stack,
                    obj_type: StorageType::Vla { elem_size: 1 },
                    symbol_key: None,
                    stack_offset: Some(offset),
                    is_tls: false,
                }
            },
            ObjectIdentity::Global { resolved_key } => {
                let g = self.globals_raw.get(resolved_key);
                let ty = g.map(|g| g.ty.clone()).unwrap_or(crate::ast::Type::Int);
                let is_tls = g.map(|g| g.is_tls).unwrap_or(false);
                let class = if is_tls {
                    StorageClass::GlobalTLS
                } else {
                    StorageClass::GlobalData
                };
                StorageInfo {
                    class,
                    obj_type: crate::semantics::storage_type_from_ast(&ty),
                    symbol_key: Some(resolved_key.clone()),
                    stack_offset: None,
                    is_tls,
                }
            },
            ObjectIdentity::Upvar => {
                let offset = self.upvar_offsets.get(name).copied().unwrap_or(0);
                StorageInfo {
                    class: StorageClass::Upvar,
                    obj_type: StorageType::Pointer { pointee_size: 8 },
                    symbol_key: None,
                    stack_offset: Some(offset),
                    is_tls: false,
                }
            },
        }
    }

    fn is_local_name(&self, name: &str) -> bool {
        self.var_map.contains_key(name)
            || self.vla_offsets.contains_key(name)
            || (self.current_func_is_nested && self.upvar_offsets.contains_key(name))
    }
}
