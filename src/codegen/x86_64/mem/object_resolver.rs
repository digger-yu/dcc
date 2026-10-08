// src/codegen/x86_64/mem/object_resolver.rs
//
// `ObjectResolver` trait 实现：lvalue 地址计算的后端无关层（crate::semantics）
// 通过它查询标识符的存储类别。原先寄居在迁移期的 stubs.rs 里，stubs.rs 作废后
// 按职责归位到 mem/。
#![allow(unused_imports)]

use crate::codegen::x86_64::Codegen;

// `ObjectResolver` trait 实现（lvalue 地址计算的后端无关层需要）。
// 语义与 monolith (28a9063) 的 resolve_identity_semantic /
// resolve_storage_semantic 一致：查找优先级 vla_offsets > var_map >
// upvar_offsets(嵌套) > globals（经 resolve_global_key，含 static local）。
// （旧顺序把 var_map 排在 vla_offsets 前面，同一个 VLA 对象因此有两套身份判据，
// 详见下面 `resolve_identity` 的注释。）
// 旧存根直查 globals_raw[name] 导致块级 static 数组被误分类为
// StackFrame{0}，元素访问从 r13+0 读垃圾（st2 类段错误）。
impl crate::semantics::ObjectResolver for Codegen {
    fn resolve_identity(&self, name: &str) -> crate::semantics::ObjectIdentity {
        use crate::semantics::ObjectIdentity;
        // `vla_offsets` 必须排在 `var_map` 前面：一个 VLA/VM 聚合对象**同时**在
        // 两张表里（`register_var` 先登记名字，`gen_vla_alloc` 再登记描述符槽），
        // 而「这个槽的内容才是对象地址」是更强的那条事实。旧顺序把它报成 Local，
        // 于是 `obj_type` 由 var_map 里那份类型快照推导 —— SSA 重建可以把它
        // 恢复成不含运行时维度的形状，`-O2` 于是把元数据槽的地址当数组基址
        // （实测 `lea -104(%r13)` 而非 `mov -104(%r13)`，与 `emit/addr.rs`、
        // `mem/access.rs::emit_ident_base_addr` 这些**先看 vla_offsets** 的站点不一致）。
        if self.vla_offsets.contains_key(name) {
            return ObjectIdentity::Vla;
        }
        if self.var_map.contains_key(name) {
            return ObjectIdentity::Local;
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
