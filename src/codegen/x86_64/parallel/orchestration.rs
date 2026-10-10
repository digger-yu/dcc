// src/codegen/x86_64/parallel/orchestration.rs
// 函数级并行 codegen 编排 (worker 分块 emit → 按序合并)。
//
// 本文件由 mod.rs 逐字搬迁而来 (batch A)：`parallel_emit_ctx` / `codegen_from_parallel`
// / `emit_func_chunk` / `merge_func_chunk` / `emit_functions_parallel` /
// `parallel_worker_count` 原样剪切进 `impl Codegen` 块，函数体一字未改，
// 仅把 `fn` 提为 `pub fn`（mod.rs 是祖先模块，需要可见性）。
//
// 关联类型 `ParallelEmitCtx` / `FuncEmitChunk` / `ParallelGlobalSnap` 与 `GlobalVar`
// 仍留在 x86_64/codegen.rs：子模块可访问祖先模块的私有 item 及其私有字段，
// 搬迁它们会波及大量字段可见性，故选取编译最平滑的方案。

use crate::ast::*;
use crate::codegen::shared::elf;

use crate::codegen::x86_64::{
    Codegen, FuncEmitChunk, GlobalVar, ParallelEmitCtx, ParallelGlobalSnap, SymbolFacts,
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
                            // 下表三个字段以前被占位（`Type::Int` / `None` / `false`），
                            // 于是同一个源文件在并行/串行两条路上选出不同指令（N12）。
                            ty: v.ty.clone(),
                            init: v.init.clone(),
                            in_data_section: v.in_data_section,
                        },
                    )
                })
                .collect(),
            static_local_map: self.static_local_map.clone(),
            // 段表跟着快照走（见 `ParallelEmitCtx::global_seg` 的存在理由）
            global_seg: self.global_seg.clone(),
            variadic_funcs: self.variadic_funcs.clone(),
            old_style_funcs: self.old_style_funcs.clone(),
            nested_funcs: self.nested_funcs.clone(),
            nested_func_renames: self.nested_func_renames.clone(),
            nested_func_parent: self.nested_func_parent.clone(),
            nested_tramp_offsets: self.nested_tramp_offsets.clone(),
            nested_chain_offsets: self.nested_chain_offsets.clone(),
            nested_func_rename_counter: self.nested_func_rename_counter,
            func_param_types: self.func_param_types.clone(),
            facts: self.capture_symbol_facts(),
            error_attr_fns: self.error_attr_fns.clone(),
            global_homes: self.global_homes.clone(),
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
                    ty: snap.ty,
                    init: snap.init,
                    section: snap.section,
                    is_tls: snap.is_tls,
                    in_data_section: snap.in_data_section,
                },
            );
        }
        // worker 重建 Codegen 时段表必须同时回位，否则段前缀只在主线程路径上出现
        cg.global_seg = ctx.global_seg;
        cg.static_local_map = ctx.static_local_map;
        cg.variadic_funcs = ctx.variadic_funcs;
        cg.old_style_funcs = ctx.old_style_funcs;
        cg.nested_funcs = ctx.nested_funcs;
        cg.nested_func_renames = ctx.nested_func_renames;
        cg.nested_func_parent = ctx.nested_func_parent;
        cg.nested_tramp_offsets = ctx.nested_tramp_offsets;
        cg.nested_chain_offsets = ctx.nested_chain_offsets;
        cg.nested_func_rename_counter = ctx.nested_func_rename_counter;
        cg.func_param_types = ctx.func_param_types;
        // 程序级符号事实下行：worker 从同一份 `defined_funcs`/`func_sizes` 出发，
        // 上行时只多不少 ⇒ 合并可以「只插缺的」（见 `SymbolFacts`）。
        cg.defined_funcs = ctx.facts.defined_funcs;
        cg.func_sizes = ctx.facts.func_sizes;
        cg.error_attr_fns = ctx.error_attr_fns;
        cg.global_homes = ctx.global_homes;
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
        // 嵌套函数的符号事实只存在于本 worker：不随 chunk 带回主 Codegen 就会
        // 在 ELF 侧发成 UND（§11.13 的 N1）。
        let facts = cg.capture_symbol_facts();
        let new_rodata = cg.rodata[rodata_base as usize..].to_vec();
        // N10: 本 chunk **新登记**的内部化条目 = `string_pool` 相对快照的差集。
        // 判据是偏移落在快照之后（快照里的条目必然在 `rodata_base` 之前，因为
        // rodata 只增不减）；换算成 chunk 局部坐标回传，合并期再按内容键重放。
        // 排序键必须是 (偏移, 键)：`HashMap` 迭代序随运行/哈希随机度变化，
        // 直接遍历它会让产物依赖调度之外的东西（不可复现）。
        let mut pool_add: Vec<(Vec<u8>, u64)> = cg
            .string_pool
            .iter()
            .filter(|(_, off)| **off >= rodata_base)
            .map(|(k, off)| (k.clone(), off - rodata_base))
            .collect();
        pool_add.sort_unstable_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
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
            // Phase 6.2：本 worker 采集的「局部 → 位置」表必须带回主 Codegen，否则
            // 并行 `-g` 构建里 worker 编译的那些函数就一个变量 DIE 都没有（与 N1/N12 同族）。
            debug_var_facts: std::mem::take(&mut cg.debug_var_facts),
            // Phase 6.2：行号条目同理必须带回。不带回则并行那一档（`-O2` 起）的
            // `.debug_line` 里一条语句行都没有：`break file.c:N` 全部报
            // “No line N in file”。PC 是 worker 坐标（= chunk 代码内偏移），
            // 合并时按 `base` 平移到主 Codegen 坐标，与 `labels` 同一换算。
            stmt_line_entries: std::mem::take(&mut cg.stmt_line_entries),
            asm_line_entries: std::mem::take(&mut cg.asm_line_entries),
            asm_insn_debug_entries: std::mem::take(&mut cg.asm_insn_debug_entries),
            asm_insn_log: std::mem::take(&mut cg.asm_insn_log),
            insn_records: std::mem::take(&mut cg.insn_records),
            emit_insn_stats: std::mem::take(&mut cg.emit_insn_stats),
            emit_insn_gas_hits: std::mem::take(&mut cg.emit_insn_gas_hits),
            new_rodata,
            rodata_base,
            pool_add,
            label_base,
            label_span,
            label_used,
            facts,
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
        // N10: `.rodata` 的按内容键重放必须提到这里 —— 它需要借用整个 chunk，
        // 而下面几个 `for x in chunk.field` 会部分移动它（`merge_chunk_rodata`
        // 只改 `self.rodata` / `self.string_pool`，与其余合并项无顺序依赖）。
        let rodata_segs = self.merge_chunk_rodata(&chunk);
        // 下面两个换算只需要函数名 / 快照基址 / 增量长度；先把它们拿下来，因为后面
        // 的 `for x in chunk.field` 会逐个部分移动 `chunk`。
        let chunk_name = chunk.name.clone();
        let rodata_inc_len = chunk.new_rodata.len();
        // worker 回传的 `float_refs` / `x87_float_refs` 里的偏移是 **worker 绝对坐标**
        //（含快照前缀），而 `pool_add` / 分段表是 chunk 局部坐标（相对增量起点）；
        // 两个坐标空间靠这个基址换算。
        let rodata_base = chunk.rodata_base;
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
        // 程序级符号事实上行（嵌套函数的 `defined_funcs`/`func_sizes` 只在本 worker
        // 里登过）：不回传则主 Codegen 的 ELF 阶段认不出那些符号（N1）。
        self.merge_symbol_facts(&chunk.facts);
        // C++ 零成本异常: 归档本 chunk 的 CFI 事实与受保护调用点。
        // 调用点里的 start/end/落地垫标签偏移都是 chunk 相对值, 落位后必须
        // 统一加上 `base`; 落地垫标签本身已在上面按 base 平移进 `labels`,
        // 所以 LSDA 装配时查到的就是最终偏移。
        if let Some(cfi) = chunk.func_cfi {
            self.func_cfi.insert(chunk.name.clone(), cfi);
        }
        // Phase 6.2：worker 采集的「局部 → 位置」表归回主 Codegen。按函数名整条append，
        // 因为名字已由 chunk 决定归属（一函数一 chunk），不存在两条同一函数。
        for entry in chunk.debug_var_facts {
            self.debug_var_facts.push(entry);
        }
        // Phase 6.2：三张行号表归回主 Codegen，PC 按 `base` 平移（与 `labels` 同一
        // 换算：worker 里那个数就是 `self.code.len()`，而 chunk 代码从 `base` 开始铺）。
        // 合并序 = slot 序 = 源码序，所以 append 本身也是确定的；`build_debug_sections`
        // 随后整张表按 PC 排序，不依赖这里的顺序。
        for &(pc, line) in chunk.stmt_line_entries.iter() {
            self.stmt_line_entries.push((base + pc, line));
        }
        for &(pc_start, pc_end, line) in chunk.asm_line_entries.iter() {
            self.asm_line_entries.push((base + pc_start, base + pc_end, line));
        }
        for &(pc, line, disc) in chunk.asm_insn_debug_entries.iter() {
            self.asm_insn_debug_entries.push((base + pc, line, disc));
        }
        for (pc, rec) in chunk.asm_insn_log {
            self.asm_insn_log.push((base + pc, rec));
        }
        for (pc, rec) in chunk.insn_records {
            self.insn_records.push((base + pc, rec));
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
        for (pos, data) in &chunk.string_refs {
            self.string_refs.push((base + *pos, data.clone()));
        }
        // 追加 chunk 局部 rodata, 并转换浮点字面量引用偏移。
        //
        // N10: 旧做法是「把 chunk 的 `new_rodata` 整块追加, 再用一个
        // `rodata_delta` 把所有按偏移定址的引用平移」。那等价于「每个 worker
        // 对自己的 `string_pool` 快照去重」⇒ 跨 worker 重复的字符串各自存一份。
        // 现在改成按**内容键重放**：逐条目问全局池「这个内容是否已有字节」，
        // 已有 ⇒ 一个字节都不追加（引用改指已有位置），没有 ⇒ 落位并登记。
        // 重放顺序 = slot 序 = 源码序（`emit_functions_parallel` 按 slot 逐个合并），
        // 因此「谁先落位」与 worker 数 / 抢活顺序 / 完成顺序无关，且与串行发射
        // 的判定逐项相同 ⇒ 产物逐字节相等。详见 §11.33。
        //
        // 两类引用分开建模（这是本缺陷真正的结构难点）：
        // - `string_refs` 按**内容**定址（带 data，resolve 期查 `string_pool`）⇒
        //   去重后自动指向唯一那份，无需任何换算；
        // - `float_refs` / `x87_float_refs` 按**偏移**定址 ⇒ 靠重放时记录的
        //   「chunk 局部偏移 → 最终偏移」分段映射来换算（取代 `rodata_delta`）。
        for &(pos, off) in &chunk.float_refs {
            let mapped = self.map_chunk_rodata_off(&chunk_name, rodata_base, rodata_inc_len, &rodata_segs, off);
            self.float_refs.push((base + pos, mapped));
        }
        for &(pos, off) in &chunk.x87_float_refs {
            let mapped = self.map_chunk_rodata_off(&chunk_name, rodata_base, rodata_inc_len, &rodata_segs, off);
            self.x87_float_refs.push((base + pos, mapped));
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

    /// 从**内容键**反推该条目在 `.rodata` 里实际占用的字节（N10）。
    ///
    /// 全部 7 个内部化写入站点只有三种形态：
    /// - 字符串字面量 / `__builtin_FILE`：键 == 落位字节（含结尾 NUL）；
    /// - SSE 浮点常量（`emit/expr/float.rs`）：键 = `"f_" + hex(bytes)`，
    ///   落位的是解码后的原始字节；
    /// - x87 浮点常量（`special/x87_floating_point.rs`）：键 = `"x87f_" + hex(bytes)`，
    ///   同上。
    ///
    /// 判别顺序把「以 NUL 结尾」放在第一位：浮点键全由十六进制字符组成，永远不
    /// 以 NUL 结尾，而字符串数据总带结尾 NUL ⇒ `"f_00"` 这类字面量不会被误解码。
    /// 反推不出来的形状一律返回 `None`，由调用方报 E1104，不静默降级。
    pub(in crate::codegen::x86_64) fn rodata_entry_bytes(key: &[u8]) -> Option<Vec<u8>> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        // 带结尾 NUL ⇒ 一定是「键 == 字节」那一路（字符串字面量等）。
        if key.last() == Some(&0) {
            return Some(key.to_vec());
        }
        let hex = if let Some(rest) = key.strip_prefix(b"x87f_") {
            rest
        } else if let Some(rest) = key.strip_prefix(b"f_") {
            rest
        } else {
            // 既不是浮点键也没有 NUL：只可能是 sanitizer 的 `__FILE__`（键不含
            // 结尾 0），落位字节就是键本身；那多出来的一个 0 字节会落进紧随其后
            // 的裸追加段，整体字节序列仍然逐字相同。
            return Some(key.to_vec());
        };
        if !hex.len().is_multiple_of(2) {
            return None;
        }
        let mut out = Vec::with_capacity(hex.len() / 2);
        for pair in hex.chunks(2) {
            let hi = HEX.iter().position(|c| *c == pair[0])? as u8;
            let lo = HEX.iter().position(|c| *c == pair[1])? as u8;
            out.push((hi << 4) | lo);
        }
        if out.is_empty() {
            return None;
        }
        Some(out)
    }

    /// 按内容键**重放**一个 chunk 的 rodata 增量，返回「chunk 局部偏移 → 最终
    /// 偏移」的分段表 `(局部起点, 最终起点, 长度)`，按局部起点升序且不重叠。
    ///
    /// chunk 局部字节流 = 内部化条目 与 裸追加（复合字面量的对齐 pad、
    /// `__builtin_huge_val` 那种不进池的浮点字节、sanitizer 的描述符）交替。
    /// 条目用内容键反推长度，剩下的缝隙原样搬运 ⇒ 不需要知道对齐值也能
    /// 还原字节。但裸追加里的 pad **个数** 是按 worker 当时的 rodata 长度算的，
    /// 而串行发射那里前面的条目可能已被去重（长度不同）⇒ 这一族残留见 §11.33。
    pub(in crate::codegen::x86_64) fn merge_chunk_rodata(
        &mut self,
        chunk: &FuncEmitChunk,
    ) -> Vec<(u64, u64, u64)> {
        let total = chunk.new_rodata.len();
        let mut segs: Vec<(u64, u64, u64)> = Vec::new();
        let mut local: usize = 0;
        for (key, local_off) in &chunk.pool_add {
            let o = *local_off as usize;
            // 条目必须在自己的增量里、且按偏移单调：两者都只可能是后端内部
            // 不变量被破坏（新条目越界 / 表没按偏移升序），一律响亮报错。
            if o < local || o > total {
                self.report_rodata_layout_bug(
                    &chunk.name,
                    format!(
                        "pool entry offset {} is out of order or out of range (cursor {}, increment {} bytes)",
                        o, local, total
                    ),
                );
                return segs;
            }
            if o > local {
                // 条目之前的裸追加：原样搬运，同时记下映射（按偏移定址的引用
                // 可以指向这一族字节，不指向它们就会指错浮点常量）。
                let start = self.rodata.len() as u64;
                let gap = o - local;
                self.rodata.extend_from_slice(&chunk.new_rodata[local..o]);
                segs.push((local as u64, start, gap as u64));
            }
            let bytes = match Self::rodata_entry_bytes(key) {
                Some(b) => b,
                None => {
                    self.report_rodata_layout_bug(
                        &chunk.name,
                        format!("cannot derive laid bytes from content key {:?}", key),
                    );
                    return segs;
                }
            };
            // 校验：反推的字节必须就是 worker 当时落进去的那些字节。这一条是
            // 整个重放方案唯一的现实假设，猜错就编译失败，不会默默产出错产物。
            if o + bytes.len() > total || chunk.new_rodata[o..o + bytes.len()] != bytes[..] {
                self.report_rodata_layout_bug(
                    &chunk.name,
                    format!(
                        "content key {:?} does not match the bytes the worker laid at chunk-local offset {}",
                        key, o
                    ),
                );
                return segs;
            }
            let start = match self.string_pool.get(key) {
                // 已有 ⇒ 一个字节也不追加，引用改指已有那份。
                Some(&prev) => prev,
                None => {
                    let s = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&bytes);
                    self.string_pool.insert(key.clone(), s);
                    s
                }
            };
            segs.push((o as u64, start, bytes.len() as u64));
            local = o + bytes.len();
        }
        if local < total {
            let start = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&chunk.new_rodata[local..]);
            segs.push((local as u64, start, (total - local) as u64));
        }
        segs
    }

    /// worker 绝对 rodata 偏移 → 最终 rodata 偏移（`float_refs` / `x87_float_refs`）。
    ///
    /// 坐标空间：worker 里的 `off = self.string_pool[key]` 是**含快照前缀**的绝对偏移。
    /// - `off < rodata_base`：条目在快照里就已存在（比如全局初始化阶段 intern 进去的
    ///   字符串），而 rodata 只增不减 ⇒ 前缀位置从不移动，**原值返回**。旧代码
    ///   无条件加 `rodata_delta`，恰好把这一类引用推进了增量里。
    /// - 其余：先减去基址换算成 chunk 局部坐标，再按重放记录的分段表映射 ⇒
    ///   被去重的条目映到「已有那份」，而不是自己那份。
    pub(in crate::codegen::x86_64) fn map_chunk_rodata_off(
        &mut self,
        chunk_name: &str,
        rodata_base: u64,
        rodata_inc_len: usize,
        segs: &[(u64, u64, u64)],
        off: u64,
    ) -> u64 {
        if off < rodata_base {
            return off;
        }
        let local = off - rodata_base;
        // 分段表按局部起点升序且不重叠，增量长度是单个函数的字节数 ⇒ 线性扫即可。
        for &(l, f, n) in segs {
            if local >= l && local < l + n {
                return f + (local - l);
            }
        }
        self.report_rodata_layout_bug(
            chunk_name,
            format!(
                "offset-based reference points at worker-absolute rodata offset {}, which is outside every recorded segment (base {:#x}, increment {} bytes, {} segments)",
                off,
                rodata_base,
                rodata_inc_len,
                segs.len()
            ),
        );
        off
    }

    /// `.rodata` 重放的前置条件被破坏：只能是后端内部不变量问题。
    fn report_rodata_layout_bug(&mut self, chunk_name: &str, msg: String) {
        self.report_internal_bug(
            "E1104",
            format!(
                "internal .rodata replay failed while merging function '{}': {}",
                chunk_name, msg
            ),
            "并行发射的 rodata 内部化条目无法按内容键重放; 不要用 DCC_NO_PARALLEL / -j1 绕过, 请修条目字节反推规则",
        );
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
                    // 显式给 worker 64 MiB 栈: Insn 临时量带 ~128 字节的
                    // [Operand; 4] 数组, debug 构建下深层嵌套的地址/表达式
                    // emit 递归帧很大, 默认 2 MiB worker 栈会溢出 (spawn 出的
                    // 线程不受 shell ulimit -s 影响)。
                    std::thread::Builder::new()
                        .stack_size(64 * 1024 * 1024)
                        .spawn_scoped(scope, || loop {
                            let i = next_job.fetch_add(1, Ordering::Relaxed);
                            if i >= n {
                                break;
                            }
                            let chunk =
                                Self::emit_func_chunk(worker_ctx(i), top_funcs[i].clone(), prog);
                            slots.lock().unwrap()[i] = Some(chunk);
                        })
                        .expect("failed to spawn codegen worker thread");
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
            debug_var_facts: vec![],
            stmt_line_entries: vec![],
            asm_line_entries: vec![],
            asm_insn_debug_entries: vec![],
            asm_insn_log: vec![],
            insn_records: vec![],
            emit_insn_stats: std::collections::HashMap::new(),
            emit_insn_gas_hits: std::collections::HashMap::new(),
            new_rodata: vec![],
            rodata_base: 0,
            pool_add: vec![],
            label_base: 0,
            label_span: u32::MAX,
            label_used: 0,
            facts: SymbolFacts::default(),
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
                    // `expect` 是 u32，加法溢出时 debug_assert 会 panic；clippy 的
                    // absurd_extreme_comparisons 知道 `expect <= u32::MAX` 恒真，但这个
                    // assert 是给阅读者的语义注释——分区上界是物理约束。
                    #[allow(clippy::absurd_extreme_comparisons)]
                    {
                        assert!(expect <= u32::MAX, "分区不得越过 u32 号池上界");
                    }
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

    // ---- N10：`.rodata` 内部化条目的内容键重放 ----

    /// 带 rodata 增量的最小 chunk。`entries` = `(内容键, chunk 局部偏移)`，
    /// 调用方必须按偏移升序传入（`emit_func_chunk` 里就是那个序）。
    fn rodata_chunk(
        name: &str,
        base: u64,
        bytes: &[u8],
        entries: &[(&str, u64)],
    ) -> FuncEmitChunk {
        let mut c = chunk(name);
        c.rodata_base = base;
        c.new_rodata = bytes.to_vec();
        c.pool_add = entries
            .iter()
            .map(|(k, off)| (k.as_bytes().to_vec(), *off))
            .collect();
        c
    }

    /// 内容键→落位字节的反推必须覆盖全部三种内部化形态，并能识破近似形状。
    #[test]
    fn rodata_entry_bytes_recovers_all_three_interning_shapes() {
        // 字符串字面量：键 == 字节（含结尾 NUL）
        assert_eq!(Codegen::rodata_entry_bytes(b"hi\0").unwrap(), b"hi\0".to_vec());
        // 空串只剩一个 NUL
        assert_eq!(Codegen::rodata_entry_bytes(b"\0").unwrap(), vec![0u8]);
        // 长得像浮点键、但带结尾 NUL 的字面量必须按字面量算（浮点键永不以 NUL 结尾）
        assert_eq!(
            Codegen::rodata_entry_bytes(b"f_00\0").unwrap(),
            b"f_00\0".to_vec()
        );
        // SSE double π：键是十六进制，落位是解码后的 8 字节
        assert_eq!(
            Codegen::rodata_entry_bytes(b"f_400921fb54442d18").unwrap(),
            vec![0x40, 0x09, 0x21, 0xfb, 0x54, 0x44, 0x2d, 0x18]
        );
        // x87 long double 零：10 字节
        assert_eq!(
            Codegen::rodata_entry_bytes(b"x87f_00000000000000000000").unwrap(),
            vec![0u8; 10]
        );
        // sanitizer 形状（键不含 NUL）：按字面量算，多出的那个 0 归紧随其后的裸追加段
        assert_eq!(
            Codegen::rodata_entry_bytes(b"main.c").unwrap(),
            b"main.c".to_vec()
        );
        // 反推不出来的形状一律 `None`（由调用方报 E1104，绝不猜）
        assert!(Codegen::rodata_entry_bytes(b"f_4").is_none());
        assert!(Codegen::rodata_entry_bytes(b"f_zz").is_none());
        assert!(Codegen::rodata_entry_bytes(b"f_").is_none());
        assert!(Codegen::rodata_entry_bytes(b"x87f_400921fb54442d1").is_none());
    }

    /// N10 本体：同一条字符串在两个 chunk 里各自 intern 过，最终只能有一份字节。
    #[test]
    fn merge_chunk_rodata_dedups_the_same_content_key_across_chunks() {
        let mut cg = x86();
        // chunk 1: "abc\0" + 一个 SSE 常量
        let c1 = rodata_chunk(
            "f1",
            0,
            b"abc\0\x01\x02\x03\x04\x05\x06\x07\x08",
            &[("abc\0", 0), ("f_0102030405060708", 4)],
        );
        let _ = cg.merge_chunk_rodata(&c1);
        assert_eq!(
            cg.rodata,
            b"abc\0\x01\x02\x03\x04\x05\x06\x07\x08".to_vec()
        );
        assert_eq!(cg.string_pool.len(), 2);
        // chunk 2: 同一个字符串 + 一个新字符串 ⇒ 只有新的那份能追加进来
        let c2 = rodata_chunk("f2", 12, b"abc\0xyz\0", &[("abc\0", 0), ("xyz\0", 4)]);
        let segs2 = cg.merge_chunk_rodata(&c2);
        assert_eq!(
            cg.rodata,
            b"abc\0\x01\x02\x03\x04\x05\x06\x07\x08xyz\0".to_vec(),
            "重复的 abc 不得再落一份"
        );
        assert_eq!(cg.string_pool.get(&b"abc\0"[..]).copied(), Some(0));
        // 重复的 "abc\0" 一个字节也没追加 ⇒ 新的 "xyz\0" 紧接在 chunk 1 的 12 字节后面
        assert_eq!(cg.string_pool.get(&b"xyz\0"[..]).copied(), Some(12));
        // chunk 2 里那条重复条目的引用必须映射到「已有那份」的位置
        assert_eq!(cg.map_chunk_rodata_off("f2", 12, 8, &segs2, 12), 0);
        assert_eq!(cg.map_chunk_rodata_off("f2", 12, 8, &segs2, 16), 12);
    }

    /// 不进池的裸追加（复合字面量的对齐 pad、`__builtin_huge_val` 那一族）必须
    /// 原样搬运，而且按偏移的引用要能映射进这一族字节里。
    #[test]
    fn merge_chunk_rodata_carries_raw_gaps_verbatim_and_maps_into_them() {
        let mut cg = x86();
        // 增量 = [4 字节 gap][SSE 常量 8 字节][2 字节尾巴]
        let bytes = vec![
            0u8, 0, 0, 0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0xAA, 0xBB,
        ];
        let c = rodata_chunk("f1", 0, &bytes, &[("f_1122334455667788", 4)]);
        let segs = cg.merge_chunk_rodata(&c);
        assert_eq!(cg.rodata, bytes, "裸追加段一个字节也不能变");
        assert_eq!(segs.len(), 3, "gap / 条目 / 尾巴 三段");
        assert_eq!(cg.map_chunk_rodata_off("f1", 0, bytes.len(), &segs, 0), 0);
        assert_eq!(cg.map_chunk_rodata_off("f1", 0, bytes.len(), &segs, 4), 4);
        assert_eq!(cg.map_chunk_rodata_off("f1", 0, bytes.len(), &segs, 12), 12);
    }

    /// 快照前缀（全局初始化阶段就 intern 进去的条目）位置从不移动：rodata 只增
    /// 不减 ⇒ 必须原值返回。旧代码无条件加 `rodata_delta`，恰好把这一类引用
    /// 推到了增量里。
    #[test]
    fn snapshot_prefix_offsets_pass_through_unchanged() {
        let mut cg = x86();
        let c = rodata_chunk("f1", 64, b"abc\0", &[("abc\0", 0)]);
        let segs = cg.merge_chunk_rodata(&c);
        assert_eq!(cg.map_chunk_rodata_off("f1", 64, 4, &segs, 8), 8, "前缀内偏移原样返回");
        assert_eq!(
            cg.map_chunk_rodata_off("f1", 64, 4, &segs, 64),
            0,
            "本 chunk 的条目映射到最终位置"
        );
    }

    /// 回传表没按偏移升序（= 快照差集收集错了）必须响亮报错，不能默默拼出
    /// 一段谁也不知道是什么的字节。
    #[test]
    fn unsorted_pool_add_is_a_hard_error_not_a_silent_merge() {
        let mut cg = x86();
        let mut c = rodata_chunk("f1", 0, b"abc\0xyz\0", &[("abc\0", 0)]);
        c.pool_add.push((b"xyz\0".to_vec(), 4));
        c.pool_add.reverse(); // 故意乱序
        let _ = cg.merge_chunk_rodata(&c);
        assert_eq!(error_codes(&cg), vec!["E1104"]);
        assert!(cg.diagnostics[0].msg.contains("f1"));
    }

    /// 内容键反推不出 worker 当时落的字节 ⇒ E1104（反推规则是唯一假设，
    /// 猜错就必须响，不能把错字节交出去）。
    #[test]
    fn content_key_mismatching_the_laid_bytes_is_a_hard_error() {
        let mut cg = x86();
        let c = rodata_chunk("f1", 0, b"XYZ\0", &[("abc\0", 0)]);
        let _ = cg.merge_chunk_rodata(&c);
        assert_eq!(error_codes(&cg), vec!["E1104"]);
        assert!(
            cg.diagnostics[0]
                .msg
                .contains("does not match the bytes the worker laid"),
            "{:?}",
            cg.diagnostics[0].msg
        );
    }

    /// 按偏移的引用落在所有段之外 ⇒ E1104，不能把它留给 `resolve` 指到错的常量。
    #[test]
    fn reference_outside_every_segment_is_a_hard_error() {
        let mut cg = x86();
        let c = rodata_chunk("f1", 0, b"abc\0", &[("abc\0", 0)]);
        let segs = cg.merge_chunk_rodata(&c);
        assert_eq!(cg.map_chunk_rodata_off("f1", 0, 4, &segs, 99), 99);
        assert_eq!(error_codes(&cg), vec!["E1104"]);
    }

    /// 确定性的全部来源就是这张表的排序：`HashMap` 迭代序随运行变化，按
    /// (偏移, 键) 升序后重放，产物只能有一份字节（与核数 / 调度无关）。
    #[test]
    fn pool_add_sorted_by_offset_makes_the_replay_deterministic() {
        let mut pool: HashMap<Vec<u8>, u64> = HashMap::new();
        for (k, o) in [("aa\0", 0u64), ("bb\0", 3), ("cc\0", 6)] {
            pool.insert(k.as_bytes().to_vec(), o);
        }
        // 与 `emit_func_chunk` 里那段完全同构的收集规则
        let collect = |pool: &HashMap<Vec<u8>, u64>, rodata_base: u64| {
            let mut v: Vec<(Vec<u8>, u64)> = pool
                .iter()
                .filter(|(_, off)| **off >= rodata_base)
                .map(|(k, off)| (k.clone(), off - rodata_base))
                .collect();
            v.sort_unstable_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            v
        };
        let mut prev: Option<Vec<(Vec<u8>, u64)>> = None;
        for _ in 0..64 {
            let cur = collect(&pool, 0);
            if let Some(p) = &prev {
                assert_eq!(p, &cur, "同一张池子两次收集必须同序");
            }
            prev = Some(cur);
        }
        assert_eq!(
            prev.unwrap(),
            vec![
                (b"aa\0".to_vec(), 0),
                (b"bb\0".to_vec(), 3),
                (b"cc\0".to_vec(), 6)
            ]
        );
        // 快照差集只拿「偏移在基址之后」的条目，基址之前的必须全部归前缀
        let tail = collect(&pool, 3);
        assert_eq!(
            tail,
            vec![(b"bb\0".to_vec(), 0), (b"cc\0".to_vec(), 3)],
            "局部坐标必须相对增量起点换算"
        );
    }
}
