// src/codegen/x86_64/stubs.rs
// 迁移过程中的临时存根：已从旧 monolithic 时代的方法在分批迁移到 emit/、mem/、
// abi/ 等子模块时丢失的部分，在此提供签名兼容的 unimplemented!() 占位，
// 以消除 E0599 "no method found" 编译错误，使剩余 300+ 错误可被编译器暴露。
//
// 迁移完成后本文件将被删除。
#![allow(dead_code)]
#![allow(unused_variables)]

use crate::ast::{BinOp, Expr, Type, UnOp, Designator, Field};
use crate::codegen::x86_64::Codegen;
use crate::debug::FuncFrame;
use std::sync::Arc;

impl Codegen {
    // ================================================================
    // 主编译入口
    // ================================================================
    pub fn compile_module(&mut self, _module: &crate::ir::Module) {
        unimplemented!("compile_module stub")
    }

    // ================================================================
    // 表达式主分派（110 处调用）
    // ================================================================
    pub fn gen_expr(&mut self, e: &Expr, dst: u8) {
        unimplemented!("gen_expr stub")
    }

    // ================================================================
    // 类型判断相关关联函数
    // ================================================================
    pub fn is_signed_int(ty: &Type) -> bool {
        matches!(ty, Type::Char | Type::SChar | Type::Short | Type::Int | Type::Long | Type::LongLong | Type::Int128)
    }

    pub fn is_integer_type(ty: &Type) -> bool {
        ty.is_int()
    }

    pub fn abi_classes(ty: &Type) -> Vec<crate::codegen::x86_64::AbiClass> {
        vec![crate::codegen::x86_64::AbiClass::Memory]
    }

    pub fn ptr_seg_prefix(ty: &Type) -> Option<u8> {
        None
    }

    pub fn member_target_ty(_ty: &Type, _is_arrow: bool) -> Type {
        Type::Int
    }

    pub fn needs_memory_return(ty: &Type) -> bool {
        ty.size() > 16
    }

    pub fn type_has_vla(ty: &Type) -> bool {
        false
    }

    pub fn fits_disp32(n: i64) -> bool {
        n >= i32::MIN as i64 && n <= i32::MAX as i64
    }

    pub fn is_int_narrow(src: &Type, dst: &Type) -> bool {
        src.size() > dst.size()
    }

    pub fn is_sseup_128_aggregate(ty: &Type) -> bool {
        false
    }

    pub fn is_fortify_builtin(name: &str) -> bool {
        false
    }

    // ================================================================
    // 向量相关
    // ================================================================
    pub fn gen_vector_load_into_slot(&mut self, _e: &Expr, _slot: i64) {
        unimplemented!("gen_vector_load_into_slot stub")
    }

    pub fn gen_vector_init_rvalue(&mut self, _inits: &[crate::ast::InitItem], _elem_ty: &Type, _n: usize, _dst_slot: i64) {
        unimplemented!("gen_vector_init_rvalue stub")
    }

    pub fn gen_vector_load(&mut self, e: &Expr) {
        unimplemented!("gen_vector_load stub")
    }

    pub fn gen_vector_store(&mut self, _name: &str) {
        unimplemented!("gen_vector_store stub")
    }

    pub fn gen_vector_binop(&mut self, _op: BinOp, _l: &Expr, _r: &Expr, _dst_slot: i64) {
        unimplemented!("gen_vector_binop stub")
    }

    pub fn gen_vector_unary_into_slot(&mut self, _op: UnOp, _e: &Expr, _elem_ty: &Type, _count: usize, _slot: i64) {
        unimplemented!("gen_vector_unary_into_slot stub")
    }

    // ================================================================
    // 内存/地址相关
    // ================================================================
    pub fn materialize_address(
        &mut self,
        desc: &crate::semantics::AddressDescriptor,
        action: crate::semantics::MaterializeAction,
        idx: Option<&Expr>,
        orig_expr: &Expr,
        base_expr: Option<&Expr>,
        dst: u8,
    ) -> bool {
        unimplemented!("materialize_address stub")
    }

    pub fn emit_load_mem_typed_seg(&mut self, sz: usize, ty: &Type, seg: u8) {
        unimplemented!("emit_load_mem_typed_seg stub")
    }

    pub fn emit_load_global_indexed(&mut self, dst: u8, idx_reg: u8, name: &str, scale: u8, ty: &Type) {
        unimplemented!("emit_load_global_indexed stub")
    }

    pub fn emit_index_scale(&mut self, elem_size: i32, arr: &Expr, elem_ty: &Type) {
        unimplemented!("emit_index_scale stub")
    }

    pub fn emit_ident_base_addr(&mut self, name: &str, ty: &Type, dst: u8) {
        unimplemented!("emit_ident_base_addr stub")
    }

    pub fn emit_global_float_load_to_xmm(&mut self, dst: u8, name: &str, ty: &Type) {
        unimplemented!("emit_global_float_load_to_xmm stub")
    }

    pub fn emit_global_float_store(&mut self, _name: &str, _ty: &Type) {
        unimplemented!("emit_global_float_store stub")
    }

    pub fn emit_global_float_load_to_xmm_offset(&mut self, dst: u8, name: &str, offset: i32, ty: &Type) {
        unimplemented!("emit_global_float_load_to_xmm_offset stub")
    }

    pub fn emit_func_addr(&mut self, dst: u8, name: &str) {
        unimplemented!("emit_func_addr stub")
    }

    pub fn emit_instrument_func_exit(&mut self) {
        unimplemented!("emit_instrument_func_exit stub")
    }

    // ================================================================
    // 结构体/聚合相关
    // ================================================================
    pub fn resolve_complete_struct_ty(&mut self, ty: &Type) -> Type {
        ty.clone()
    }

    pub fn gen_sizeof_runtime(&mut self, ty: &Type, dst: u8) {
        unimplemented!("gen_sizeof_runtime stub")
    }

    pub fn gen_block_items(&mut self, items: &[crate::ast::BlockItem]) {
        unimplemented!("gen_block_items stub")
    }

    pub fn gen_agg_elem_store(&mut self, _r: &Expr, _elem_ty: &Type, _es: i32, _dst: u8) {
        unimplemented!("gen_agg_elem_store stub")
    }

    pub fn gen_aggregate_assign(&mut self, _name: &str, _ty: &Type, _r: &Expr) {
        unimplemented!("gen_aggregate_assign stub")
    }

    pub fn gen_init_list_at(&mut self, _slot: i64, _ty: &Type, _inits: &[crate::ast::InitItem]) {
        unimplemented!("gen_init_list_at stub")
    }

    pub fn gen_global_init_list(&mut self, _ty: &Type, _inits: &[crate::ast::InitItem], _base: Option<(u64, Option<u64>)>, _use_rodata: bool) -> Option<Vec<u8>> {
        unimplemented!("gen_global_init_list stub")
    }

    pub fn gen_same_size_bitcast(&mut self, _e: &Expr, _ty: &Type, _dst: u8) -> bool {
        false
    }

    pub fn compute_offset(ty: &Type, member: &str) -> i64 {
        let _ = (ty, member);
        0
    }

    // ================================================================
    // 浮点相关
    // ================================================================
    pub fn gen_float_imm_load(&mut self, _val: f64, _ty: &Type) {
        unimplemented!("gen_float_imm_load stub")
    }

    pub fn gen_float_load(&mut self, name: &str, ty: &Type) {
        unimplemented!("gen_float_load stub")
    }

    pub fn gen_float_store(&mut self, _name: &str, _ty: &Type) {
        unimplemented!("gen_float_store stub")
    }

    pub fn gen_float_store_at(&mut self, _slot: i64, _ty: &Type) {
        unimplemented!("gen_float_store_at stub")
    }

    pub fn gen_float_binop(&mut self, _op: BinOp, _l: &Expr, _r: &Expr) {
        unimplemented!("gen_float_binop stub")
    }

    pub fn emit_truth_flags(&mut self, ty: &Type) {
        unimplemented!("emit_truth_flags stub")
    }

    pub fn emit_implicit_float_to_int(&mut self, _dst: u8, _src_ty: &Type, _dst_ty: &Type) {
        unimplemented!("emit_implicit_float_to_int stub")
    }

    pub fn emit_float_to_int_saturate(&mut self, _dst: u8, _src_ty: &Type, _wide: bool) {
        unimplemented!("emit_float_to_int_saturate stub")
    }

    pub fn emit_float_to_ulong(&mut self, _dst: u8, _is_double: bool) {
        unimplemented!("emit_float_to_ulong stub")
    }

    pub fn emit_ulong_to_float(&mut self, _dst: u8, _is_double: bool) {
        unimplemented!("emit_ulong_to_float stub")
    }

    pub fn emit_truncate_to_unsigned(&mut self, _dst: u8, _ty: &Type) {
        unimplemented!("emit_truncate_to_unsigned stub")
    }

    pub fn emit_zero_extend(&mut self, _dst: u8, _ty: &Type) {
        unimplemented!("emit_zero_extend stub")
    }

    pub fn emit_float_precision_convert(&mut self, _src_ty: &Type, _dst_ty: &Type) {
        unimplemented!("emit_float_precision_convert stub")
    }

    pub fn store_complex_at_rcx(&mut self, _r: &Expr, _ty: &Type) {
        unimplemented!("store_complex_at_rcx stub")
    }

    pub fn gen_complex_store_rcx(&mut self, _r: &Expr, _ty: &Type) {
        unimplemented!("gen_complex_store_rcx stub")
    }

    pub fn gen_complex_sse_return(&mut self, _e: &Expr, _ty: &Type) {
        unimplemented!("gen_complex_sse_return stub")
    }

    pub fn gen_complex_imag_into_xmm0(&mut self, _r: &Expr, _ty: &Type) {
        unimplemented!("gen_complex_imag_into_xmm0 stub")
    }

    pub fn gen_complex_binop(&mut self, _op: BinOp, _l: &Expr, _r: &Expr, _dst: u8) {
        unimplemented!("gen_complex_binop stub")
    }

    pub fn gen_imaginary_binop(&mut self, _op: BinOp, _l: &Expr, _r: &Expr, _dst: u8) {
        unimplemented!("gen_imaginary_binop stub")
    }

    // ================================================================
    // VLA / 变长类型相关
    // ================================================================
    pub fn emit_vla_restore(&mut self) {
        unimplemented!("emit_vla_restore stub")
    }

    pub fn gen_vla_alloc(&mut self, _v: &crate::ast::VarDecl) {
        unimplemented!("gen_vla_alloc stub")
    }

    pub fn vla_bound_size_slot(&mut self, _e: &Expr) -> Option<i64> {
        None
    }

    pub fn vm_size_slot(&mut self, _ty: &Type) -> Option<i64> {
        None
    }

    pub fn gen_vm_struct_bind(&mut self, _sd: &crate::ast::StructDef, _is_union: bool) {
        unimplemented!("gen_vm_struct_bind stub")
    }

    pub fn gen_vm_type_bind(&mut self, _ty: &Type) {
        unimplemented!("gen_vm_type_bind stub")
    }

    pub fn gen_vm_member_offset(&mut self, _ty: &Type, _field: &str, _dst: u8) -> bool {
        false
    }

    pub fn block_contains_vla(items: &[crate::ast::BlockItem]) -> bool {
        let _ = items;
        false
    }

    // ================================================================
    // 嵌套函数 / 静态链
    // ================================================================
    pub fn resolve_nested_func_ref(&mut self, name: &str) -> Option<(String, usize, Vec<i64>)> {
        None
    }

    pub fn load_trampoline_addr(&mut self, dst: u8, name: &str, hops: usize, offs: &[i64]) {
        unimplemented!("load_trampoline_addr stub")
    }

    pub fn load_upvar(&mut self, dst: u8, off: i64, sz: usize, name: &str) {
        unimplemented!("load_upvar stub")
    }

    pub fn store_upvar(&mut self, _src: u8, _off: i64, _sz: usize, _name: &str) {
        unimplemented!("store_upvar stub")
    }

    pub fn load_static_chain_for_call(&mut self, _hops: usize, _offs: &[i64]) {
        unimplemented!("load_static_chain_for_call stub")
    }

    // ================================================================
    // 标签 / 汇编相关
    // ================================================================
    pub fn c_label_to_asm(name: &str) -> String {
        format!(".L{}", name)
    }

    pub fn asm_replace_label_idx(s: &str, _idx: usize, _label: &str) -> String {
        s.to_string()
    }

    pub fn build_asm_output(&mut self, prog: &crate::ast::Program) -> String {
        String::new()
    }

    pub fn func_frames(&mut self, module: &crate::ir::Module) -> Vec<FuncFrame> {
        vec![]
    }

    // ================================================================
    // ASM 块相关（asm/emit.rs 引用）
    // ================================================================
    pub fn build_asm_operand_map(
        &mut self,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
        bindings: &mut Vec<crate::codegen::x86_64::asm::emit::Binding>,
    ) -> std::collections::HashMap<String, (String, u8)> {
        std::collections::HashMap::new()
    }

    pub fn merge_standalone_prefixes(lines: Vec<String>) -> Vec<String> {
        lines
    }

    pub fn resolve_asm_template(
        &mut self,
        line: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        inputs: &[Expr],
        input_names: &[Option<String>],
        op_map: &std::collections::HashMap<String, (String, u8)>,
    ) -> String {
        line.to_string()
    }

    pub fn emit_resolved_asm_lines(&mut self, lines: &[String], is_goto: bool) {
        unimplemented!("emit_resolved_asm_lines stub")
    }

    pub fn writeback_asm_operand(
        &mut self,
        expr: &Expr,
        reg: u8,
        xmm_width: u16,
    ) {
        unimplemented!("writeback_asm_operand stub")
    }

    // ================================================================
    // 表达式工具函数
    // ================================================================
    pub fn exprs_structurally_equal(a: &Expr, b: &Expr) -> bool {
        false
    }

    pub fn expr_bitfield_width(e: &Expr) -> Option<u32> {
        None
    }

    pub fn expr_has_side_effects(e: &Expr) -> bool {
        true
    }

    pub fn expr_warn(&mut self, e: &Expr, msg: &str) {
        unimplemented!("expr_warn stub")
    }

    pub fn find_offsetof_field_type(ty: &Type, field: &str) -> Option<Type> {
        let _ = (ty, field);
        None
    }

    pub fn stmt_ends_with_return(stmt: &crate::ast::Stmt) -> bool {
        false
    }

    pub fn stmt_contains_label(stmt: &crate::ast::Stmt) -> bool {
        false
    }

    pub fn try_gen_fp_contract(&mut self, _op: BinOp, _l: &Expr, _r: &Expr, _dst: u8) -> bool {
        false
    }

    pub fn mask_imm_to_type(v: i128, _ty: &Type) -> i128 {
        v
    }

    pub fn mov_to_stack(&mut self, _src: u8, _name: &str) {
        unimplemented!("mov_to_stack stub")
    }

    pub fn mov_from_stack(&mut self, dst: u8, name: &str) {
        unimplemented!("mov_from_stack stub")
    }

    pub fn resolve_va_list_addr(&mut self, e: &Expr) -> crate::codegen::x86_64::VaListAddress {
        crate::codegen::x86_64::VaListAddress::Named(String::new())
    }

    pub fn gen_va_arg(&mut self, addr: crate::codegen::x86_64::VaListAddress, ty: &Type, dst: u8) {
        unimplemented!("gen_va_arg stub")
    }

    pub fn gen_va_arg_ptr(&mut self, ty: &Type, dst: u8) {
        unimplemented!("gen_va_arg_ptr stub")
    }

    pub fn gen_va_arg_struct(&mut self, addr: crate::codegen::x86_64::VaListAddress, ty: &Type, dst: u8) {
        unimplemented!("gen_va_arg_struct stub")
    }

    pub fn gen_va_arg_ptr_struct(&mut self, ty: &Type, dst: u8) {
        unimplemented!("gen_va_arg_ptr_struct stub")
    }

    pub fn gen_int_promote(&mut self, _dst: u8, _ty: &Type) {
        unimplemented!("gen_int_promote stub")
    }

    pub fn gen_int_narrow_cast(&mut self, _dst: u8, _ty: &Type) {
        unimplemented!("gen_int_narrow_cast stub")
    }

    pub fn gen_sret_return(&mut self, _e: &Expr, _ty: &Type) {
        unimplemented!("gen_sret_return stub")
    }

    pub fn gen_builtin(&mut self, name: &str, args: &[Expr], dst: u8) -> bool {
        false
    }

    pub fn gen_atomic_builtin(&mut self, name: &str, args: &[Expr], dst: u8) -> bool {
        false
    }

    pub fn gen_unknown_builtin_stmt(&mut self, e: &Expr) -> bool {
        false
    }

    pub fn gen_unknown_builtin_fallback(&mut self, name: &str, _ty: &Type, _dst: u8, _sret_off: Option<i64>) {
        unimplemented!("gen_unknown_builtin_fallback stub")
    }

    pub fn gen_compound_assign_single_eval(&mut self, _lhs: &Expr, _rhs: &Expr, _op: BinOp, _dst: u8) {
        unimplemented!("gen_compound_assign_single_eval stub")
    }

    pub fn ensure_custom_section(&mut self, name: &str, _exec: bool, _writable: bool) -> usize {
        0
    }

    pub fn compile_func(&mut self, func: &crate::ast::FuncDecl, _prog: &crate::ast::Program) {
        unimplemented!("compile_func stub")
    }

    pub fn emit_func_epilogue(&mut self) {
        unimplemented!("emit_func_epilogue stub")
    }

    pub fn asm_warn(&mut self, msg: &str) {
        unimplemented!("asm_warn stub")
    }

    pub fn binop(&mut self, _op: BinOp, _dst: u8, _src: u8, _signed: bool, _lty: &Type, _rty: &Type) {
        unimplemented!("binop stub")
    }

    pub fn pos(&self) -> u64 {
        self.code.len() as u64
    }
}

// ObjectResolver trait 实现（lvalue 地址计算的后端无关层需要）
impl crate::semantics::ObjectResolver for Codegen {
    fn resolve_identity(&self, name: &str) -> crate::semantics::ObjectIdentity {
        if self.var_map.contains_key(name) {
            crate::semantics::ObjectIdentity::Local
        } else if self.globals_raw.contains_key(name) {
            let resolved_key = self
                .resolve_global_key(name)
                .unwrap_or_else(|| name.to_string());
            crate::semantics::ObjectIdentity::Global { resolved_key }
        } else {
            crate::semantics::ObjectIdentity::Upvar
        }
    }

    fn resolve_storage(
        &self,
        id: &crate::semantics::ObjectIdentity,
        name: &str,
    ) -> crate::semantics::StorageInfo {
        use crate::semantics::{StorageClass, StorageType};
        match id {
            crate::semantics::ObjectIdentity::Local => {
                if let Some((off, ty)) = self.var_map.get(name) {
                    let st = StorageType::Scalar { size: ty.size() };
                    crate::semantics::StorageInfo {
                        class: StorageClass::Stack,
                        obj_type: st,
                        symbol_key: None,
                        stack_offset: Some(*off),
                        is_tls: false,
                    }
                } else {
                    crate::semantics::StorageInfo {
                        class: StorageClass::Stack,
                        obj_type: StorageType::Scalar { size: 8 },
                        symbol_key: None,
                        stack_offset: Some(0),
                        is_tls: false,
                    }
                }
            }
            crate::semantics::ObjectIdentity::Global { resolved_key } => {
                if let Some(gv) = self.globals_raw.get(resolved_key) {
                    let st = StorageType::Scalar { size: gv.size };
                    crate::semantics::StorageInfo {
                        class: StorageClass::GlobalData,
                        obj_type: st,
                        symbol_key: Some(resolved_key.clone()),
                        stack_offset: None,
                        is_tls: gv.is_tls,
                    }
                } else {
                    crate::semantics::StorageInfo {
                        class: StorageClass::GlobalData,
                        obj_type: StorageType::Scalar { size: 8 },
                        symbol_key: Some(resolved_key.clone()),
                        stack_offset: None,
                        is_tls: false,
                    }
                }
            }
            _ => crate::semantics::StorageInfo {
                class: StorageClass::Stack,
                obj_type: StorageType::Scalar { size: 8 },
                symbol_key: None,
                stack_offset: None,
                is_tls: false,
            },
        }
    }

    fn is_local_name(&self, name: &str) -> bool {
        self.var_map.contains_key(name)
    }
}
