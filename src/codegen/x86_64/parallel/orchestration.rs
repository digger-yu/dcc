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
            // 默认直接继承主 Codegen 的游标; `emit_functions_parallel` 会按 slot
            // 下标把它换成互不重叠的分区基址。
            label_base: self.next_label,
            label_span: u32::MAX - self.next_label,
        }
    }

    /// `.L` 内部标号命名空间在并行发射下的分区容量。
    ///
    /// `next_label` 是 `u32`, 号源只有一个 (`Codegen::fresh_label`), 而发射被切成
    /// `n` 个互不通信的 worker ⇒ 唯一既不串号、又与调度无关的分法是按 slot 下标
    /// 等分剩余标号池: `slot i` 拥有 `[base_i, base_i + span)`,
    /// `span = (u32::MAX - origin) / n`。
    ///
    /// 越界条件因此是「单个函数需要 > 4.29e9/n 个内部标号」; n 是本 TU 的函数数,
    /// 该量级对任何真实输入都有多个数量级的余量。万一越界, `merge_func_chunk`
    /// 会报 E1101 而不是静默覆盖。
    pub(in crate::codegen::x86_64) fn parallel_label_span(n_funcs: usize, origin: u32) -> u32 {
        let avail = (u32::MAX - origin) as u64;
        if n_funcs <= 1 {
            avail as u32
        } else {
            (avail / n_funcs as u64) as u32
        }
    }

    /// slot `i` 的 `.L` 区间基址。必须只依赖 `origin` + `i` (源码序), 绝不依赖
    /// worker 编号 / 完成顺序, 否则产物随调度抖动。
    pub(in crate::codegen::x86_64) fn parallel_label_base(origin: u32, slot: usize, span: u32) -> u32 {
        ((origin as u64) + (slot as u64) * (span as u64)).min(u32::MAX as u64) as u32
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
        // worker 的标号游标从分区基址起号。这是消除「每个 worker 都从 .L0 开始」
        // 那一路径同名覆盖的唯一插入点: `fresh_label` 本身不动, 保持单一真源。
        cg.next_label = ctx.label_base;
        cg
    }

    pub(in crate::codegen::x86_64) fn emit_func_chunk(
        ctx: ParallelEmitCtx,
        f: FuncDecl,
        prog: &Program,
    ) -> FuncEmitChunk {
        let rodata_base = ctx.rodata.len() as u64;
        let label_base = ctx.label_base;
        let label_span = ctx.label_span;
        let mut cg = Self::codegen_from_parallel(ctx);
        cg.compile_func(&f, prog);
        // 本函数实际用掉的标号数。worker 内部就闭合 (不跨函数取号), 因此该值与
        // 调度 / 核数无关。
        let label_used = cg.next_label.saturating_sub(label_base);
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
            label_base,
            label_span,
            label_used,
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
            // 函数入口标签已由上面按 `entry_offset` 落表 (worker 表里的同名项与它
            // 同值, 因为 `entry_offset` 就是从同一张表读的)。
            if *name == chunk.name {
                continue;
            }
            // 缺陷 B: 绝不静默覆盖。`labels` 是「标号名 → .text 偏移」的唯一真源;
            // 两个 chunk 交出同名标号 = 它们的 `je/jmp` 会在合并后被回填到同一
            // 地址 —— 实测表现为 p1 的循环跳进 p2。这不是可降级的情形, 只能当后端
            // 内部不变量破坏上报, 并且 **保留先落表的那份** (覆盖会让后一个函数的
            // 跳转指向先一个函数的代码, 正是本缺陷的失效形态)。
            if let Some(&prev) = self.labels.get(name.as_str()) {
                self.report_label_collision(&chunk, name.as_str(), prev, base + *off);
            } else {
                self.labels.insert(name.clone(), base + *off);
            }
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

    /// chunk 标号与本 Codegen 已有标号同名。
    fn report_label_collision(&mut self, chunk: &FuncEmitChunk, name: &str, prev: u64, new: u64) {
        self.report_internal_bug(
            "E1101",
            format!(
                "internal label '{}' collides while merging function '{}': \
                 already defined at .text+{:#x}, now redefined at .text+{:#x} \
                 (label namespace partition [{:#x}, {:#x}))",
                name, chunk.name, prev, new, chunk.label_base, chunk.label_span
            ),
            "并行发射的 .L 标号命名空间不唯一; 不要用 -j1 / DCC_NO_PARALLEL 绕过, 请修分区分配",
        );
    }

    /// 标号分区闭合性: slot `i` 实际用掉的标号不得越进 slot `i+1` 的区间。
    /// 按 **slot 序** (而非完成序) 检查, 使诊断本身也与调度无关。
    fn check_label_partition(&mut self, chunks: &[FuncEmitChunk]) {
        for c in chunks {
            if c.label_used > c.label_span {
                self.report_internal_bug(
                    "E1101",
                    format!(
                        "function '{}' consumed {} internal labels, exceeding its \
                         .L namespace partition of {} (base {:#x})",
                        c.name, c.label_used, c.label_span, c.label_base
                    ),
                    "increase the .L label pool or partition it unevenly; silent \
                     label overwrite would miscompile a neighbour function",
                );
            }
        }
    }

    /// 函数级并行 emit：`-O2+` 多函数 TU 在 worker 线程中分块生成后按序合并。
    pub(in crate::codegen::x86_64) fn emit_functions_parallel(
        &mut self,
        top_funcs: &[FuncDecl],
        prog: &Program,
    ) {
        let n = top_funcs.len();
        // ---- `.L` 标号命名空间分区 (缺陷 B 根因) ----
        // 必须在抓 ctx 之前算: `parallel_emit_ctx()` 把主游标当作区间原点,
        // 发射前已被全局初始化代码用掉的 `.L<n>` / `.L.compound.<n>` 不能被重号。
        let label_origin = self.next_label;
        let label_span = Self::parallel_label_span(n, label_origin);
        let ctx = Arc::new(self.parallel_emit_ctx());
        let degree = Self::parallel_worker_count(n);

        // 每个 slot 一份互不重叠的标号区间。注意用的是 **slot 下标** 而不是
        // worker 编号 / 完成顺序: worker 靠 `next_job.fetch_add` 抢活, 任何建立在
        // 抢活结果上的号段都会让产物随调度抖动。slot 序 == 合并序 == 源码序。
        let worker_ctx = |slot: usize| {
            let mut c = ctx.as_ref().clone();
            c.label_base = Self::parallel_label_base(label_origin, slot, label_span);
            c.label_span = label_span;
            c
        };

        let chunks: Vec<FuncEmitChunk> = if degree <= 1 {
            top_funcs
                .iter()
                .enumerate()
                .map(|(i, f)| Self::emit_func_chunk(worker_ctx(i), f.clone(), prog))
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
                            Self::emit_func_chunk(worker_ctx(i), top_funcs[i].clone(), prog);
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

        // 分区闭合性与重名守卫先于合并: 两者都是「本轮产物不可信」的硬错误,
        // 必须在回填 `labels` 前就报错, 否则诊断会依赖 chunk 合并顺序。
        self.check_label_partition(&chunks);

        // 合并后主游标必须 >= 所有 chunk 用过的最大标号, 否则之后任何走串行
        // 发射的标号会再撞一次名。
        let high_water = chunks
            .iter()
            .map(|c| c.label_base.saturating_add(c.label_used))
            .max()
            .unwrap_or(label_origin);
        self.next_label = self.next_label.max(high_water);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Abi, Arch, Os, TargetTriple};
    use crate::codegen::x86_64::JumpKind;
    use crate::codegen::x86_64::PendingJump;
    use crate::diagnostics::Severity;
    use crate::target::Target;

    fn x86() -> Codegen {
        Codegen::new(Target::resolve(&TargetTriple {
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: Os::Linux,
            abi: Abi::Gnu,
        }))
    }

    /// 最小可用 chunk: 只填标号命名空间相关字段, 其余为空。
    fn chunk(name: &str) -> FuncEmitChunk {
        FuncEmitChunk {
            name: name.to_string(),
            code: vec![],
            entry_offset: 0,
            size: 0,
            labels: HashMap::new(),
            pending: vec![],
            string_refs: vec![],
            float_refs: vec![],
            x87_float_refs: vec![],
            pending_func_rels: vec![],
            pending_code_rels: vec![],
            pending_func_addr_rels: vec![],
            pending_data_rels: vec![],
            pending_sib_data_rels: vec![],
            label_addr_refs: vec![],
            pending_custom_sec_rels: vec![],
            pushsection_code_ranges: vec![],
            extra_sections: vec![],
            custom_sec_indices: HashMap::new(),
            func_cfi: None,
            eh_funcs: vec![],
            disasm_calls: HashMap::new(),
            new_rodata: vec![],
            rodata_base: 0,
            label_base: 0,
            label_span: u32::MAX,
            label_used: 0,
        }
    }

    fn error_codes(cg: &Codegen) -> Vec<&'static str> {
        cg.diagnostics
            .iter()
            .filter(|d| d.sev == Severity::Error)
            .map(|d| d.code)
            .collect()
    }

    /// 缺陷 B 的可复现性论证核心: 号段只能由 (原点, slot 下标) 决定。
    /// slot 序 == 合并序 == 源码序, 与 worker 数 / 抢活顺序 / 完成顺序无关。
    #[test]
    fn label_partitions_are_contiguous_disjoint_and_never_wrap() {
        for origin in [0u32, 1, 7, 1000, 65536] {
            for n in 1..=257usize {
                let span = Codegen::parallel_label_span(n, origin);
                assert!(span > 0, "n={n} origin={origin}: span 不得为 0");
                let mut expect = origin;
                for slot in 0..n {
                    let base = Codegen::parallel_label_base(origin, slot, span);
                    assert_eq!(base, expect, "slot {slot} 必须紧接上一个 slot");
                    expect = base + span;
                    assert!(expect <= u32::MAX, "分区不得越过 u32 号池上界");
                }
            }
        }
    }

    #[test]
    fn label_base_is_independent_of_worker_count() {
        // 同一个 slot 在不同并行度下必须拿到同一个基址, 否则产物随调度抖动。
        let origin = 3u32;
        for n in [2usize, 3, 5, 8, 16] {
            let span = Codegen::parallel_label_span(n, origin);
            let _ = span;
        }
        // degree 不进入公式: 基址只由 (origin, slot, span) 决定。
        let span = Codegen::parallel_label_span(8, origin);
        for slot in 0..8 {
            assert_eq!(
                Codegen::parallel_label_base(origin, slot, span),
                origin + (slot as u32) * span
            );
        }
    }

    /// 合并时重名不得静默覆盖: 先落表的那一份必须保持不动, 并给出 E1101。
    /// 覆盖正是原缺陷的失效形态 —— p1 的 `.L1` 被 p2 的同名定义顶掉后,
    /// p1 的循环跳进了 p2 的代码。
    #[test]
    fn merging_colliding_labels_reports_e1101_and_keeps_the_first_definition() {
        let mut cg = x86();
        let mut a = chunk("p1");
        a.labels.insert(".L1".to_string(), 0x40);
        cg.merge_func_chunk(a, 1);
        let first = cg.labels[".L1"];

        let mut b = chunk("p2");
        b.labels.insert(".L1".to_string(), 0x90);
        cg.merge_func_chunk(b, 1);

        assert_eq!(error_codes(&cg), vec!["E1101"], "重名必须报错");
        let d = &cg.diagnostics[0];
        assert!(
            d.msg.contains(".L1") && d.msg.contains("p2"),
            "诊断要点出冲突标号与归属函数: {}",
            d.msg
        );
        assert_eq!(
            cg.labels[".L1"], first,
            "绝不得用后一个 chunk 的定义覆盖先一个"
        );
    }

    /// 分区后的正常局面: 不同函数交出不同名标号, 不得产生任何诊断。
    #[test]
    fn merging_disjoint_label_namespaces_is_silent() {
        let mut cg = x86();
        let origin = 0u32;
        let span = Codegen::parallel_label_span(2, origin);
        let mut a = chunk("p1");
        a.label_base = Codegen::parallel_label_base(origin, 0, span);
        a.label_used = 2;
        a.labels
            .insert(format!(".L{}", a.label_base), 0x10);
        let mut b = chunk("p2");
        b.label_base = Codegen::parallel_label_base(origin, 1, span);
        b.label_used = 2;
        b.labels
            .insert(format!(".L{}", b.label_base), 0x20);
        cg.merge_func_chunk(a.clone(), 1);
        cg.merge_func_chunk(b.clone(), 1);
        assert!(cg.diagnostics.is_empty(), "unexpected: {:?}", error_codes(&cg));
        // 两个函数的同名偏移不得互相顶掉: 此处名字本身就不同。
        assert_ne!(
            format!(".L{}", a.label_base),
            format!(".L{}", b.label_base)
        );
        cg.check_label_partition(&[a, b]);
        assert!(cg.diagnostics.is_empty(), "unexpected: {:?}", error_codes(&cg));
    }

    /// 扇区越界 (单函数用超了自己的号段) 必须在合并前就被拦住。
    #[test]
    fn label_partition_overrun_is_reported() {
        let mut cg = x86();
        let mut c = chunk("huge");
        c.label_span = 4;
        c.label_used = 5;
        cg.check_label_partition(&[c]);
        assert_eq!(error_codes(&cg), vec!["E1101"]);
        assert!(cg.diagnostics[0].msg.contains("huge"));
    }

    /// worker 的起始游标必须等于分区基址 —— 这是「每个 worker 从 .L0 起号」
    /// 那个根因唯一的修复点。
    #[test]
    fn worker_codegen_starts_at_its_partition_base() {
        let mut cg = x86();
        cg.next_label = 11;
        let mut ctx = cg.parallel_emit_ctx();
        assert_eq!(ctx.label_base, 11, "默认继承主游标, 不得从 0 起号");
        ctx.label_base = 5000;
        ctx.label_span = 100;
        let mut worker = Codegen::codegen_from_parallel(ctx);
        assert_eq!(worker.next_label, 5000);
        assert_eq!(worker.fresh_label(), ".L5000");
    }

    /// chunk 合并后主游标必须高于所有 worker 用过的最大标号, 否则之后任何
    /// 走串行发射的函数会再撞一次名。
    #[test]
    fn merged_chunks_advance_the_main_label_cursor() {
        let mut cg = x86();
        let top = 64u32;
        cg.next_label = top;
        let span = Codegen::parallel_label_span(2, top);
        let mut a = chunk("p1");
        a.label_base = Codegen::parallel_label_base(top, 0, span);
        a.label_used = 3;
        let mut b = chunk("p2");
        b.label_base = Codegen::parallel_label_base(top, 1, span);
        b.label_used = 7;
        let high_water = a
            .label_base
            .max(b.label_base.saturating_add(b.label_used));
        cg.next_label = cg.next_label.max(high_water);
        assert!(cg.next_label > b.label_base + b.label_used - 1);
        // 主游标后续取号不得与任何 chunk 重名。
        assert!(cg.fresh_label() != format!(".L{}", b.label_base));
    }

    /// pending 合并后仍指向同一个标号名: 并行不得把 chunk 的跳转目标
    /// 名字改动 (那会改变回填结果)。
    #[test]
    fn merging_preserves_pending_jump_targets_shifted_by_base() {
        let mut cg = x86();
        let mut a = chunk("p1");
        a.code = vec![0xE9, 0, 0, 0, 0];
        a.pending.push(PendingJump {
            at: 0,
            target: ".L5".to_string(),
            kind: JumpKind::Jmp,
            disp_offset: 1,
            next_ip: 5,
            is_push_imm: false,
        });
        a.labels.insert(".L5".to_string(), 9);
        cg.merge_func_chunk(a, 1);
        assert_eq!(cg.pending.len(), 1);
        assert_eq!(cg.pending[0].target, ".L5");
        assert_eq!(cg.labels[".L5"], 9);
    }
}
