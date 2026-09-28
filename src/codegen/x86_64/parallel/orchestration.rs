// src/codegen/x86_64/parallel/orchestration.rs
// 函数级并行 codegen 编排 (worker 分块 emit → 按序合并)。
//
// 本文件由 mod.rs 逐字搬迁而来 (batch A)：`parallel_emit_ctx` / `codegen_from_parallel`
// / `emit_func_chunk` / `merge_func_chunk` / `emit_functions_parallel` /
// `parallel_worker_count` 原样剪切进 `impl Codegen` 块，函数体一字未改，
// 仅把 `fn` 提为 `pub fn`（mod.rs 是祖先模块，需要可见性）。
//
// 关联类型 `ParallelEmitCtx` / `FuncEmitChunk` / `ParallelGlobalSnap` 与 `GlobalVar`
// 仍留在 x86_64/mod.rs：子模块可访问祖先模块的私有 item 及其私有字段，
// 搬迁它们会波及大量字段可见性，故选取编译最平滑的方案。

use crate::ast::*;
use crate::codegen::shared::elf;

use crate::codegen::x86_64::{
    Codegen, FuncEmitChunk, GlobalVar, ParallelEmitCtx, ParallelGlobalSnap,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

impl Codegen {
    pub(in crate::codegen::x86_64) fn parallel_emit_ctx(&self) -> ParallelEmitCtx {
        ParallelEmitCtx {
            target: self.target.clone(),
            relocatable: self.relocatable,
            function_sections: self.function_sections,
            emit_asm_text: self.emit_asm_text,
            debug_level: self.debug_level,
            emit_cfi: self.emit_cfi,
            source_name: self.source_name.clone(),
            line_map: self.line_map.clone(),
            globals: self
                .globals_raw
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        ParallelGlobalSnap {
                            offset: v.offset,
                            size: v.size,
                            section: v.section,
                            is_tls: v.is_tls,
                        },
                    )
                })
                .collect(),
            variadic_funcs: self.variadic_funcs.clone(),
            old_style_funcs: self.old_style_funcs.clone(),
            nested_funcs: self.nested_funcs.clone(),
            nested_func_renames: self.nested_func_renames.clone(),
            nested_func_parent: self.nested_func_parent.clone(),
            nested_tramp_offsets: self.nested_tramp_offsets.clone(),
            nested_chain_offsets: self.nested_chain_offsets.clone(),
            nested_func_rename_counter: self.nested_func_rename_counter,
            func_param_types: self.func_param_types.clone(),
            defined_funcs: self.defined_funcs.clone(),
            error_attr_fns: self.error_attr_fns.clone(),
            global_reg_bindings: self.global_reg_bindings.clone(),
            rodata: self.rodata.clone(),
            string_pool: self.string_pool.clone(),
            text_align: self.text_align,
        }
    }

    pub(in crate::codegen::x86_64) fn codegen_from_parallel(ctx: ParallelEmitCtx) -> Self {
        let mut cg = Self::new(ctx.target);
        cg.relocatable = ctx.relocatable;
        cg.function_sections = ctx.function_sections;
        cg.emit_asm_text = ctx.emit_asm_text;
        cg.debug_level = ctx.debug_level;
        cg.emit_cfi = ctx.emit_cfi;
        cg.source_name = ctx.source_name;
        cg.line_map = ctx.line_map;
        for (name, snap) in ctx.globals {
            cg.globals_raw.insert(
                name,
                GlobalVar {
                    offset: snap.offset,
                    size: snap.size,
                    ty: Type::Int,
                    init: None,
                    section: snap.section,
                    is_tls: snap.is_tls,
                    in_data_section: false,
                },
            );
        }
        cg.variadic_funcs = ctx.variadic_funcs;
        cg.old_style_funcs = ctx.old_style_funcs;
        cg.nested_funcs = ctx.nested_funcs;
        cg.nested_func_renames = ctx.nested_func_renames;
        cg.nested_func_parent = ctx.nested_func_parent;
        cg.nested_tramp_offsets = ctx.nested_tramp_offsets;
        cg.nested_chain_offsets = ctx.nested_chain_offsets;
        cg.nested_func_rename_counter = ctx.nested_func_rename_counter;
        cg.func_param_types = ctx.func_param_types;
        cg.defined_funcs = ctx.defined_funcs;
        cg.error_attr_fns = ctx.error_attr_fns;
        cg.global_reg_bindings = ctx.global_reg_bindings;
        cg.rodata = ctx.rodata;
        cg.string_pool = ctx.string_pool;
        cg.text_align = ctx.text_align;
        cg
    }

    pub(in crate::codegen::x86_64) fn emit_func_chunk(
        ctx: ParallelEmitCtx,
        f: FuncDecl,
        prog: &Program,
    ) -> FuncEmitChunk {
        let rodata_base = ctx.rodata.len() as u64;
        let mut cg = Self::codegen_from_parallel(ctx);
        cg.compile_func(&f, prog);
        let new_rodata = cg.rodata[rodata_base as usize..].to_vec();
        let size = cg
            .func_sizes
            .get(&f.name.name)
            .copied()
            .unwrap_or(cg.code.len() as u64);
        let entry_offset = cg.labels.get(&f.name.name).copied().unwrap_or(0);
        FuncEmitChunk {
            name: f.name.name.clone(),
            code: cg.code,
            entry_offset,
            size,
            labels: cg.labels,
            pending: cg.pending,
            string_refs: cg.string_refs,
            float_refs: cg.float_refs,
            x87_float_refs: cg.x87_float_refs,
            pending_func_rels: cg.pending_func_rels,
            pending_code_rels: cg.pending_code_rels,
            pending_func_addr_rels: cg.pending_func_addr_rels,
            pending_data_rels: cg.pending_data_rels,
            pending_sib_data_rels: cg.pending_sib_data_rels,
            label_addr_refs: cg.label_addr_refs,
            pending_custom_sec_rels: cg.pending_custom_sec_rels,
            pushsection_code_ranges: cg.pushsection_code_ranges,
            extra_sections: cg.extra_sections,
            custom_sec_indices: cg.custom_sec_indices,
            // C++ 零成本异常: CFI 事实与受保护调用点必须随 chunk 带回主 Codegen,
            // 否则并行产物与串行产物不一致 (不可复现), 且 FDE 的 CFA 规则会退回
            // 旧的常量段 —— unwinder 走不出本帧。
            func_cfi: cg.func_cfi.get(&f.name.name).copied(),
            eh_funcs: cg.eh_funcs,
            disasm_calls: cg.disasm_calls,
            new_rodata,
            rodata_base,
        }
    }

    pub(in crate::codegen::x86_64) fn merge_func_chunk(
        &mut self,
        chunk: FuncEmitChunk,
        func_align: usize,
    ) {
        while !self.code.len().is_multiple_of(func_align) {
            self.code.push(0x90);
        }
        let base = self.pos();
        self.labels
            .insert(chunk.name.clone(), base + chunk.entry_offset);
        for (name, off) in &chunk.labels {
            self.labels.insert(name.clone(), base + *off);
        }
        self.func_sizes.insert(chunk.name.clone(), chunk.size);
        // C++ 零成本异常: 归档本 chunk 的 CFI 事实与受保护调用点。
        // 调用点里的 start/end/落地垫标签偏移都是 chunk 相对值, 落位后必须
        // 统一加上 `base`; 落地垫标签本身已在上面按 base 平移进 `labels`,
        // 所以 LSDA 装配时查到的就是最终偏移。
        if let Some(cfi) = chunk.func_cfi {
            self.func_cfi.insert(chunk.name.clone(), cfi);
        }
        for info in chunk.eh_funcs {
            let name = info.name.clone();
            // chunk 内的调用点与函数起点都是 chunk 相对值, 落位后统一加 `base`。
            let text_start = info.text_start + base;
            let sites: Vec<_> = info
                .sites
                .into_iter()
                .map(|mut s| {
                    s.start += base;
                    s.end += base;
                    s
                })
                .collect();
            if sites.is_empty() {
                continue;
            }
            // 同一函数名在并行 chunk 里只会出现一次; 若已有条目 (串行路径已经
            // 归档过同名函数), 合并调用点而不是覆盖。归属判据是「同名字 + 同一段
            // 发射地址」: 地址不同就是两次独立发射, 合并会把两组调用点编到同一个
            // LSDA 里, 而 FDE 只能描述其中一份代码。
            if let Some(existing) = self
                .eh_funcs
                .iter_mut()
                .find(|f| f.name == name && f.text_start == text_start)
            {
                existing.sites.extend(sites);
            } else {
                self.eh_funcs
                    .push(crate::codegen::x86_64::eh_tables::EhFuncInfo {
                        name,
                        text_start,
                        sites,
                    });
            }
        }
        self.code.extend_from_slice(&chunk.code);
        for (pos, data) in chunk.string_refs {
            self.string_refs.push((base + pos, data));
        }
        // 追加 chunk 局部 rodata, 并转换浮点字面量引用偏移。
        // 每个 worker 从 ctx.rodata 快照开始, 记录的是该局部视图里的
        // rodata 偏移, 返回时只携带 new_rodata。此处先把新字节追加到
        // 全局 rodata, 再用 rodata_delta 把所有局部偏移映射为最终
        // .rodata 偏移。SSE 与 x87 引用都走同一规则; 如果按是否来自
        // 基础池做特殊分支, 后续 PC32 重定位会指向错误浮点常量。
        let rodata_delta = self.rodata.len() as u64 - chunk.rodata_base;
        self.rodata.extend_from_slice(&chunk.new_rodata);
        for (pos, off) in chunk.float_refs {
            self.float_refs.push((base + pos, off + rodata_delta));
        }
        for (pos, off) in chunk.x87_float_refs {
            self.x87_float_refs.push((base + pos, off + rodata_delta));
        }
        for mut p in chunk.pending {
            p.at += base;
            p.disp_offset += base;
            p.next_ip += base;
            self.pending.push(p);
        }
        for (pos, sym) in chunk.pending_func_rels {
            self.pending_func_rels.push((base + pos, sym));
        }
        for mut rel in chunk.pending_code_rels {
            rel.offset += base;
            self.pending_code_rels.push(rel);
        }
        for (pos, sym, is_lea) in chunk.pending_func_addr_rels {
            self.pending_func_addr_rels.push((base + pos, sym, is_lea));
        }
        for (pos, sym) in chunk.pending_sib_data_rels {
            self.pending_sib_data_rels.push((base + pos, sym));
        }
        for (pos, sym) in chunk.pending_data_rels {
            self.pending_data_rels.push((base + pos, sym));
        }
        for (pos, lbl) in chunk.label_addr_refs {
            self.label_addr_refs.push((base + pos, lbl));
        }
        let mut sec_map = HashMap::new();
        // old_chunk_idx → (global_sec_idx, data_offset_delta)
        let mut sec_data_delta: Vec<(usize, u64)> = Vec::new();
        for extra in chunk.extra_sections {
            let old_idx = sec_map.len();
            let sec_idx = self.ensure_custom_section(
                &extra.name,
                extra.flags & elf::SHF_EXECINSTR != 0,
                extra.flags & elf::SHF_WRITE != 0,
            );
            self.extra_sections[sec_idx].align =
                self.extra_sections[sec_idx].align.max(extra.align);
            // Append chunk section data directly (no alignment padding)
            // SHF_MERGE sections require exact entry size multiples.
            let delta = self.extra_sections[sec_idx].data.len() as u64;
            self.extra_sections[sec_idx]
                .data
                .extend_from_slice(&extra.data);
            sec_map.insert(old_idx, sec_idx);
            sec_data_delta.push((old_idx, delta));
        }
        for mut rel in chunk.pending_custom_sec_rels {
            if let Some(&sec_idx) = sec_map.get(&rel.sec_idx) {
                let delta = sec_data_delta
                    .iter()
                    .find(|(old, _)| *old == rel.sec_idx)
                    .map(|(_, d)| *d)
                    .unwrap_or(0);
                rel.sec_idx = sec_idx;
                rel.offset += delta;
                self.pending_custom_sec_rels.push(rel);
            }
        }
        for (start, size, old_sec_idx, sec_off) in chunk.pushsection_code_ranges {
            if let Some(&sec_idx) = sec_map.get(&old_sec_idx) {
                let delta = sec_data_delta
                    .iter()
                    .find(|(old, _)| *old == old_sec_idx)
                    .map(|(_, d)| *d)
                    .unwrap_or(0);
                self.pushsection_code_ranges
                    .push((base + start, size, sec_idx, sec_off + delta));
            }
        }
        for (pos, name) in chunk.disasm_calls {
            self.disasm_calls.insert(base + pos, name);
        }
    }

    /// 函数级并行 emit：`-O2+` 多函数 TU 在 worker 线程中分块生成后按序合并。
    pub(in crate::codegen::x86_64) fn emit_functions_parallel(
        &mut self,
        top_funcs: &[FuncDecl],
        prog: &Program,
    ) {
        let ctx = Arc::new(self.parallel_emit_ctx());
        let n = top_funcs.len();
        let degree = Self::parallel_worker_count(n);

        let chunks: Vec<FuncEmitChunk> = if degree <= 1 {
            top_funcs
                .iter()
                .map(|f| Self::emit_func_chunk(ctx.as_ref().clone(), f.clone(), prog))
                .collect()
        } else {
            let next_job = AtomicUsize::new(0);
            let slots = Mutex::new(vec![None; n]);
            std::thread::scope(|scope| {
                for _ in 0..degree {
                    scope.spawn(|| loop {
                        let i = next_job.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        let chunk =
                            Self::emit_func_chunk(ctx.as_ref().clone(), top_funcs[i].clone(), prog);
                        slots.lock().unwrap()[i] = Some(chunk);
                    });
                }
            });
            slots
                .into_inner()
                .expect("parallel codegen mutex poisoned")
                .into_iter()
                .map(|c| c.expect("parallel codegen dropped a function chunk"))
                .collect()
        };

        for (f, chunk) in top_funcs.iter().zip(chunks.iter()) {
            debug_assert_eq!(f.name.name, chunk.name);
            let func_align = f.alignment.unwrap_or(1).max(1);
            self.merge_func_chunk(chunk.clone(), func_align);
        }
    }

    /// 并行 worker 数量：逻辑核心数与函数数取较小值，至少 1。
    pub(in crate::codegen::x86_64) fn parallel_worker_count(n_funcs: usize) -> usize {
        if n_funcs <= 1 {
            return 1;
        }
        std::thread::available_parallelism()
            .map(|p| p.get().min(n_funcs))
            .unwrap_or(4)
            .max(1)
    }
}
