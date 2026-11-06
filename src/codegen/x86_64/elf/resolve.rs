// src/codegen/x86_64/elf/resolve.rs
// ET_EXEC/ET_REL final symbol resolution and relocation patching.

use crate::ast::{Decl, Program};
use crate::codegen::shared::elf::{
    ElfRela, ExtraRelaSection, R_X86_64_32S, R_X86_64_64, R_X86_64_GOTPCREL, R_X86_64_PC32,
    R_X86_64_PLT32, R_X86_64_TPOFF32, STB_LOCAL, STT_GNU_IFUNC,
};
use crate::codegen::x86_64::{Codegen, PendingCodeRel};

impl Codegen {
    /// 「符号名 → symtab 索引」的唯一解析路径 (缺陷 D)。
    ///
    /// 旧代码在 9 个重定位发射点各写一遍
    /// `asm_label_map.get(..).cloned().unwrap_or(name)` +
    /// `sym_index.get(..).copied().unwrap_or(0)` (其中 3 个连 `asm_label_map`
    /// 归一化都没做, 形态还各不相同)。`unwrap_or(0)` 把「符号不存在」变成
    /// `STN_UNDEF`: 链接期按 ELF 规格取 `S = 0`, 重定位静默落到镜像起点,
    /// 产出一个「能编过、能链上、跑必错」的目标文件。ET_EXEC 分支为了同一件事
    /// 早就报 E1100, 只有 `-c` 这条 ET_REL 路径不报错 —— 本函数把两条路径拉平:
    /// 归一化只一条路径, 缺失只一个报错点。
    ///
    /// 返回 `(symtab 索引, 需要并进 addend 的尾部位移)`；返回 `None` 时调用方必须
    /// **跳过这条重定位并让诊断说话** (诊断是 `Severity::Error`, 会在 `codegen::emit`
    /// 的 fatal 检查处终止编译), 绝不拿 0 当兼底值继续往下发。
    ///
    /// 位移剥解也放在这里 (`sym±N`, 见 [`split_sym_offset`])：内联 asm 的内存操作数与
    /// `.pushsection` 数据表达式都会把 `boot_cpu_data+114` 这种**整串**当成符号名交下来。
    /// 此前只有 `pending_data_rels` 和自定义节两条出口自己拆过一次, 而实测这类名字的来源
    /// 有十一条 pending 列表、出口同样有十来个 —— 漏一条出口就是全树 417 处
    /// `undefined reference to 'boot_cpu_data+114'`。收进唯一解析路径才不再需要「记住每条
    /// 出口」；建表侧 (`rel_symbols.rs::referenced_reloc_symbols`) 用同一个函数归一化,
    /// 保证 symtab 里只有基名那一条。
    fn reloc_sym_index(&mut self, name: &str, site: &str) -> Option<(u32, i64)> {
        // 热路径: 先整名查, 不为此分配字符串。
        let hit = match self.asm_label_map.get(name) {
            Some(resolved) => self.sym_index.get(resolved).copied(),
            None => self.sym_index.get(name).copied(),
        };
        if let Some(idx) = hit {
            return Some((idx, 0));
        }
        // 冷路径: `sym±N` —— 基名命中就把位移交回调用方并进 addend。
        let (base, off) = split_sym_offset(name);
        if off != 0 && base != name {
            let hit = match self.asm_label_map.get(base) {
                Some(resolved) => self.sym_index.get(resolved).copied(),
                None => self.sym_index.get(base).copied(),
            };
            if let Some(idx) = hit {
                return Some((idx, off as i64));
            }
        }
        // 冷路径: 同一缺失符号被多个使用点碰到时会产生完全相同的 msg,
        // 由 `diagnostics::dedup_diags` 在输出端去重。
        let resolved = self
            .asm_label_map
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string());
        if Self::is_internal_label(&resolved) {
            self.report_internal_bug(
                "E1103",
                format!(
                    "relocation site '{}' references internal label '{}' which has no entry in \
                     this TU's symtab",
                    site, resolved
                ),
                "a compiler-generated .L label must be resolved by the emitter, not turned into \
                 a relocation against STN_UNDEF (index 0)",
            );
        } else {
            self.report_internal_bug(
                "E1103",
                format!(
                    "unresolved symbol '{}' at relocation site '{}': ET_REL output cannot emit a \
                     relocation without a symtab entry (index 0 would silently resolve to the \
                     section start)",
                    resolved, site
                ),
                "the symbol table builder (elf/rel_symbols.rs) must register every referenced \
                 name as a UND / defined symbol before relocations are emitted",
            );
        }
        None
    }

    /// 把 `.L.str.const.<hash>` 形式的编译器内部 label 反向解析为 rodata 偏移.
    ///
    /// 内部 label 不进 symtab (GAS 约定), 所以自定义节里的 `.long .L.str.const.N`
    /// 不能走普通的 `reloc_sym_index`. 正确做法是把这条重定位改成针对 `.rodata`
    /// section symbol, addend 填字符串在 rodata 里的偏移.
    ///
    /// 反向查表: label 里的 hash 就是 `const_str_id(data)`, 遍历 string_pool 找
    /// 匹配 data. string_pool 在真实内核 TU 里通常 < 几百条, 线性扫可控.
    fn lookup_str_const_label_offset(
        label: &str,
        string_pool: &std::collections::HashMap<Vec<u8>, u64>,
    ) -> Option<u64> {
        let hash = label.strip_prefix(".L.str.const.")?;
        let hash: u64 = hash.parse().ok()?;
        for (data, &off) in string_pool.iter() {
            if crate::const_eval::const_str_id(data) == hash {
                return Some(off);
            }
        }
        None
    }

    pub fn resolve(&mut self, rodata_start: u64, data_start: u64) {
        for (p, name) in &self.pending_data_rels {
            self.disasm_data.insert(*p, name.clone());
        }
        // 修补跳转；ET_REL 下最终仍未解析的符号降级为 PC32 relocation。
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            let Some(&dst) = self.labels.get(&p.target) else {
                if self.relocatable {
                    // `push $sym` 的 imm32 是绝对地址 (R_X86_64_32S)；普通跳转是 PC32。
                    let (rel_type, addend) = if p.is_push_imm {
                        (R_X86_64_32S, 0)
                    } else {
                        (R_X86_64_PC32, -4)
                    };
                    let rel_off = p.disp_offset;
                    self.pending_code_rels.push(PendingCodeRel {
                        offset: rel_off,
                        symbol: p.target,
                        rel_type,
                        addend,
                    });
                    continue;
                }
                // ET_EXEC 模式下未解析的符号: 发出诊断错误而非 panic.
                // 外部符号 (如 libc 函数) 应由 driver 层的 needs_host_cc_link
                // 委托 host cc 链接; 若到达此处说明绕过了 host cc 回退.
                self.diagnostics.push(crate::diagnostics::Diagnostic {
                    sev: crate::diagnostics::Severity::Error,
                    code: "E1100",
                    msg: format!(
                        "unresolved symbol '{}': ET_EXEC mode cannot \
                         resolve external symbols; use -c for relocatable \
                         output or link with host cc",
                        p.target
                    ),
                    span: crate::diagnostics::Span::point("<link>", 0, 0),
                    hint: Some("compile with -c or use host cc for linking".into()),
                    source_line: None,
                });
                continue;
            };
            let off = p.disp_offset as usize;
            if p.is_push_imm {
                // push $label: imm32 = 绝对地址（低 32 位）。ET_EXEC 的代码偏移 0
                // 对应虚拟地址 `self.entry`；直接写 `dst` 会把一个节内偏移当地址压栈
                // （见 codegen.rs `patch_local_jumps` 同名分支的现场说明）。
                let addr = self.entry + dst;
                self.code[off..off + 4].copy_from_slice(&(addr as i32).to_le_bytes());
            } else {
                let rel = (dst as i64) - (p.next_ip as i64);
                let rel_off = p.disp_offset as usize;
                self.code[rel_off..rel_off + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补 RIP-relative 字符串引用 (LEA dst, [RIP+disp32])
        for (code_pos, data) in self.string_refs.drain(..) {
            let str_off = self.string_pool[&data];
            let str_vaddr = rodata_start + str_off;
            let rip = self.entry + code_pos + 7; // LEA is 7 bytes
            let rel = (str_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 3; // disp32 starts at byte 3
            self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 RIP-relative 浮点字面量引用 (movsd/movss xmm0, [RIP+disp32])
        for (code_pos, rodata_off) in self.float_refs.drain(..) {
            let flt_vaddr = rodata_start + rodata_off;
            let rip = self.entry + code_pos + 8; // movsd/movss is 8 bytes
            let rel = (flt_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 4; // disp32 starts at byte 4
            self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 x87 fld [RIP+disp32] 引用 (6 字节指令, disp32 @ byte 2)
        for (code_pos, rodata_off) in self.x87_float_refs.drain(..) {
            let flt_vaddr = rodata_start + rodata_off;
            let rip = self.entry + code_pos + 6; // fld [rip+disp32] is 6 bytes
            let rel = (flt_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 2; // disp32 starts at byte 2
            self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 -fbounds-check ubsan 诊断数据 (LEA rdi, [RIP+disp32] + 指针)
        // ET_EXEC: 镜像在单个 RWX 段, rodata 运行时可写, 直接修补。
        // ET_REL: OutOfBoundsData 在 .data, 由 compile_rel 生成重定位。
        if !self.relocatable {
            for (code_pos, data_off) in self.ubsan_lea_refs.drain(..) {
                let data_vaddr = rodata_start + data_off;
                let rip = self.entry + code_pos + 7; // LEA = 7 bytes
                let rel = (data_vaddr as i64) - (rip as i64);
                let p = code_pos as usize + 3; // disp32 starts at byte 3
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
            for (data_off, field_off, target_off) in self.ubsan_ptr_refs.drain(..) {
                let target_vaddr = rodata_start + target_off;
                // rodata 已拼接进 self.code (exec_text_size 之后), 修补副本
                let fp = (self.exec_text_size + data_off + field_off) as usize;
                self.code[fp..fp + 8].copy_from_slice(&target_vaddr.to_le_bytes());
            }
        }
        // 修补 RIP-relative 全局变量引用 (mov/lea reg, [RIP+disp32])
        for (disp32_pos, var_name) in self.pending_data_rels.drain(..) {
            if let Some(g) = self.globals_raw.get(&var_name) {
                if g.is_tls {
                    continue;
                }
                let var_vaddr = if g.in_data_section {
                    data_start + g.offset
                } else {
                    rodata_start + g.offset
                };
                let rip = self.entry + disp32_pos + 4;
                let rel = (var_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补带元素偏移的全局引用 (数组型 static local 逐元素运行时初始化)
        for (disp32_pos, var_name, elem_off) in self.pending_data_rels_with_off.drain(..) {
            if let Some(g) = self.globals_raw.get(&var_name) {
                if g.is_tls {
                    continue;
                }
                let base_vaddr = if g.in_data_section {
                    data_start + g.offset
                } else {
                    rodata_start + g.offset
                };
                let var_vaddr = (base_vaddr as i64 + elem_off as i64) as u64;
                let rip = self.entry + disp32_pos + 4;
                let rel = (var_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补 SIB base=101 全局访问 (disp32 绝对寻址) - 直接写符号绝对地址
        for (disp32_pos, var_name) in self.pending_sib_data_rels.drain(..) {
            if let Some(g) = self.globals_raw.get(&var_name) {
                let var_vaddr = if g.in_data_section {
                    data_start + g.offset
                } else {
                    rodata_start + g.offset
                };
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(var_vaddr as u32).to_le_bytes());
            }
        }
        // 修补 TLS 变量 RIP-relative 引用（ET_EXEC .tdata 模板）
        let tdata_start = rodata_start + self.rodata.len() as u64;
        for (disp32_pos, var_name) in self.pending_tls_rels.drain(..) {
            // __dcc_tls_base 是合成符号: 指向 TLS 模板起始地址
            if var_name == "__dcc_tls_base" {
                let rip = self.entry + disp32_pos + 4;
                let rel = (tdata_start as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
                continue;
            }
            if let Some(g) = self.globals_raw.get(&var_name) {
                let var_vaddr = tdata_start + g.offset;
                let rip = self.entry + disp32_pos + 4;
                let rel = (var_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补字符串初始化指针 (char *s = "hello"): 指针槽里的 rodata 偏移
        // 替换为绝对地址 (rodata 已拼接到 self.code 的 exec_text_size 之后)。
        for name in &self.rodata_string_init_globals {
            if let Some(g) = self.globals_raw.get(name) {
                let code_off = (self.exec_text_size + g.offset) as usize;
                if code_off + 8 <= self.code.len() {
                    let str_off =
                        u64::from_le_bytes(self.code[code_off..code_off + 8].try_into().unwrap());
                    let abs_addr = rodata_start + str_off;
                    self.code[code_off..code_off + 8].copy_from_slice(&abs_addr.to_le_bytes());
                }
            }
        }
        self.rodata_string_init_globals.clear();
        // 修补 InitList 数组/结构体中的字符串初始化指针槽 (compile_exec 路径):
        // 槽内写入的是 str_off 占位 (emit_string_ptr_slot), 替换为绝对地址。
        for slot_off in std::mem::take(&mut self.rodata_string_slots) {
            let code_off = (self.exec_text_size + slot_off) as usize;
            if code_off + 8 <= self.code.len() {
                let str_off =
                    u64::from_le_bytes(self.code[code_off..code_off + 8].try_into().unwrap());
                let abs_addr = rodata_start + str_off;
                self.code[code_off..code_off + 8].copy_from_slice(&abs_addr.to_le_bytes());
            }
        }
        // 修补 &&label 地址引用 (LEA reg, [RIP+disp32])
        for (disp32_pos, label_name) in self.label_addr_refs.drain(..) {
            if let Some(&label_off) = self.labels.get(&label_name) {
                let label_vaddr = self.entry + label_off;
                let rip = self.entry + disp32_pos + 4;
                let rel = (label_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补 data 节里指向本 TU 文本标号的 64 位槽（static local 跳转表，
        // 如 Lua `luaV_execute` 的 `disptab`）。槽里现在是 0 占位；标号偏移在
        // `self.labels`（相对代码段起点），加段基址即运行期绝对地址 —— 与
        // `label_addr_refs` 同一个换算，只是写在 data 而不是 code。
        for (data_off, label_name, addend) in std::mem::take(&mut self.pending_data_textlabel_rels)
        {
            let Some(&label_off) = self.labels.get(&label_name) else {
                continue;
            };
            let code_off = self.exec_text_size
                + self.rodata.len() as u64
                + self.tdata_section.len() as u64
                + data_off;
            let p = code_off as usize;
            if p + 8 <= self.code.len() {
                let vaddr = self.entry + label_off + addend as u64;
                self.code[p..p + 8].copy_from_slice(&vaddr.to_le_bytes());
            }
        }
    }

    /// ET_REL Phase 3 (自 monolith compile_rel 尾部恢复): 符号表定稿后
    /// 把所有 pending_* 引用转换为最终 ElfRela 条目 + init/fini_array 节
    /// + 自定义节重定位搬迁。由 compile_rel 在 collect_exec_symbols 后调用。
    pub fn emit_rel_relocations(
        &mut self,
        prog: &Program,
        custom_func_ranges: &[(u64, u64, usize, u64, u64)],
    ) {
        // .pushsection 非文本节: 创建重定位条目
        //
        // **必须是 BTreeMap 而非 HashMap**: 下方 `for (sec_idx, relocations) in ...` 的
        // 输出序直接决定 `.rela.<custom>` 在 `extra_rela_sections` 里的 push 序，
        // 即最终 **ELF 节头表的节排列顺序**。用 HashMap 时同一输入会产生 k! 种节序，
        // 产物不可复现。key 是 `sec_idx` (usize)，天然有序且与源码声明序一致。
        let mut custom_sec_relas: std::collections::BTreeMap<usize, Vec<ElfRela>> =
            std::collections::BTreeMap::new();
        for rel in std::mem::take(&mut self.pending_custom_sec_rels) {
            // 编译器内部的字符串字面量 label (.L.str.const.<hash>) 在自定义节里
            // 被引用时 (内联汇编 %c 把字面量 label 嵌入 .pushsection), 不注册到
            // symtab —— 这是 GAS 的约定: 这类 label 是 rodata 里的位置标记,
            // 不应当作独立符号发 UND 重定位. 正确做法是把它替换成对 .rodata
            // section symbol 的重定位, addend = 字符串在 rodata 内的偏移.
            if let Some(rodata_off) =
                Self::lookup_str_const_label_offset(&rel.symbol, &self.string_pool)
            {
                let Some((rodata_sym, _)) =
                    self.reloc_sym_index(".rodata", ".rela<custom section> (str.const)")
                else {
                    continue;
                };
                let abs_offset = rel.offset;
                let rela = ElfRela {
                    offset: abs_offset,
                    sym_index: rodata_sym,
                    rel_type: rel.rel_type,
                    // 原 addend 保留 (表达式可能在 label 后再带 ±N).
                    addend: rodata_off as i64 + rel.addend,
                };
                custom_sec_relas.entry(rel.sec_idx).or_default().push(rela);
                continue;
            }
            // 自定义节（`.altinstr_aux`/`.altinstr_replacement`/`__ex_table` …）里的重定位
            // 同样可能引用 `sym±N` 这种内联 asm 写法（实测全树 417 处
            // `undefined reference to 'boot_cpu_data+100'`，站点在 `main.o:(.altinstr_aux+0x2)`）。
            // 剥位移已收进 [`reloc_sym_index`] 本身，这里只需把它返回的位移并进 addend。
            let Some((sym_idx, sym_off)) = self.reloc_sym_index(&rel.symbol, ".rela<custom section>")
            else {
                continue;
            };
            let abs_offset = rel.offset;
            let rela = ElfRela {
                offset: abs_offset,
                sym_index: sym_idx,
                rel_type: rel.rel_type,
                addend: rel.addend + sym_off,
            };
            custom_sec_relas.entry(rel.sec_idx).or_default().push(rela);
        }
        for (sec_idx, relocations) in custom_sec_relas {
            let rela_name = format!(".rela{}", self.extra_sections[sec_idx].name);
            let target_sec_index = 7 + sec_idx as u32;
            if let Some(existing) = self
                .extra_rela_sections
                .iter_mut()
                .find(|r| r.target_sec_index == target_sec_index)
            {
                existing.relocations.extend(relocations);
            } else {
                self.extra_rela_sections.push(ExtraRelaSection {
                    name: rela_name,
                    relocations,
                    target_sec_index,
                });
            }
        }

        // Phase 3: 构建重定位 (使用正确的符号索引)
        //
        // 节符号 (`.rodata` / `.data`) 由 rel_symbols 在对应节非空时才创建, 所以
        // 不在这里统一提前解析: 没有字符串/浮点引用时 `.rodata` 本就可以不存在,
        // 提前解析会把一个合法局面报成错误。每个使用点自己走 `reloc_sym_index`,
        // 缺失只在那里说。

        // Patch __mcount_loc relocations with .text section symbol index
        if self
            .extra_rela_sections
            .iter()
            .any(|r| r.name == ".rela__mcount_loc")
        {
            if let Some((text_sym_idx, _)) = self.reloc_sym_index(".text", ".rela__mcount_loc") {
                for rela_sec in &mut self.extra_rela_sections {
                    if rela_sec.name == ".rela__mcount_loc" {
                        for rel in &mut rela_sec.relocations {
                            rel.sym_index = text_sym_idx;
                        }
                    }
                }
            }
        }

        for (code_pos, data) in std::mem::take(&mut self.string_refs) {
            let Some((sym_idx, _)) = self.reloc_sym_index(".rodata", "string literal PC32") else {
                continue;
            };
            let str_off = self.string_pool[&data] as i64;
            self.relocations.push(ElfRela {
                offset: code_pos + 3,
                sym_index: sym_idx,
                rel_type: R_X86_64_PC32,
                // LEA 7 bytes, disp32 at byte 3.
                // RIP = code_pos+7, P = code_pos+3.
                // R_X86_64_PC32: disp32 = S + A - P.
                // effective addr = RIP + disp32 = S + A + 4.
                // target = S + str_off, so A = str_off - 4.
                addend: str_off - 4,
            });
        }

        // 浮点字面量重定位
        for (code_pos, rodata_off) in std::mem::take(&mut self.float_refs) {
            let Some((sym_idx, _)) = self.reloc_sym_index(".rodata", "SSE float literal PC32")
            else {
                continue;
            };
            // movsd/movss xmm, [RIP+disp32] 是 8 字节指令, disp32 在 +4。
            // ELF64 R_X86_64_PC32: disp32 = S + A - P, 其中 P 是
            // 重定位字段地址 code_pos + 4。CPU 运行时从下一条 RIP
            // (code_pos + 8) 加 disp32, 有效地址就是 S + A + 4。
            // 目标地址为 .rodata + rodata_off, 因此 A = rodata_off - 4。
            self.relocations.push(ElfRela {
                offset: code_pos + 4,
                sym_index: sym_idx,
                rel_type: R_X86_64_PC32,
                addend: rodata_off as i64 - 4,
            });
        }
        // x87 fld 字面量重定位 (6 字节指令, disp32 @ byte 2)
        for (code_pos, rodata_off) in std::mem::take(&mut self.x87_float_refs) {
            let Some((sym_idx, _)) = self.reloc_sym_index(".rodata", "x87 float literal PC32")
            else {
                continue;
            };
            self.relocations.push(ElfRela {
                offset: code_pos + 2,
                sym_index: sym_idx,
                rel_type: R_X86_64_PC32,
                // fld 6 字节, disp32 在 +2, 下一条 RIP 是 code_pos + 6。
                // P = code_pos + 2, 所以有效地址仍是 S + A + 4。
                addend: rodata_off as i64 - 4,
            });
        }

        // -fbounds-check ubsan 诊断数据重定位:
        // LEA rdi, [rip+disp32] → PC32 重定位 (指向 .data 内 OutOfBoundsData;
        // libubsan SourceLocation::acquire() 原子写 Column, .rodata 只读会段错误)
        for (code_pos, data_off) in std::mem::take(&mut self.ubsan_lea_refs) {
            let Some((sym_idx, _)) = self.reloc_sym_index(".data", "ubsan OutOfBoundsData PC32")
            else {
                continue;
            };
            self.relocations.push(ElfRela {
                offset: code_pos + 3,
                sym_index: sym_idx,
                rel_type: R_X86_64_PC32,
                // LEA 7 bytes, disp32 at byte 3.
                // effective addr = S + A + 4, A = data_off - 4.
                addend: data_off as i64 - 4,
            });
        }
        // data 内绝对指针 (Filename@0, &ArrayType@16, &IndexType@24)
        // → R_X86_64_64 指向 .rodata 内 TypeDescriptor / 文件名字符串
        for (data_off, field_off, target_off) in std::mem::take(&mut self.ubsan_ptr_refs) {
            let Some((sym_idx, _)) = self.reloc_sym_index(".rodata", "ubsan TypeDescriptor R_64")
            else {
                continue;
            };
            self.data_relocations.push(ElfRela {
                offset: data_off + field_off,
                sym_index: sym_idx,
                rel_type: R_X86_64_64,
                addend: target_off as i64,
            });
        }

        // 解析函数调用重定位 (本 TU 内的 static/本地函数用 PC32,
        // 跨 TU 外部调用用 PLT32 以兼容 PIE/动态链接).
        for (code_off, func_name) in std::mem::take(&mut self.pending_func_rels) {
            let Some((sym_idx, sym_off)) = self.reloc_sym_index(&func_name, "call PLT32/PC32")
            else {
                continue;
            };
            // 检查符号绑定: LOCAL 表示本 TU 定义 (含 static), 用 PC32
            // 产生位置无关重定位, 避免 PLT 跳转
            let (is_local, is_ifunc) = self
                .symbols
                .get(sym_idx as usize)
                .map(|s| (s.bind == STB_LOCAL, s.ty == STT_GNU_IFUNC))
                .unwrap_or((false, false));
            // IFUNC 必须走 PLT32，**即使是 STB_LOCAL 基名**：PC32 是对符号地址的
            // 静态绑定，而 IFUNC 符号的 `st_value` 是 **resolver 体**，把它直接当
            // 调用目标 = 每次都跳到 resolver 并拿返回地址当代码跳——真正会跑的只有
            // PLT（由 ld 为 IFUNC 生成 PLT 条目 + 一条 `R_X86_64_IRELATIVE`）。
            // 实测 gcc 13.3 对 `static` + target_clones 也发 `R_X86_64_PLT32 fstatic - 4`
            // （符号表里 `fstatic` 是 IFUNC LOCAL），所以「LOCAL 就用 PC32」这条启发
            // 式对 IFUNC 不成立。（取数过程见 docs/wp_ifunc_real_fix.md）
            let rel_type = if is_local && !is_ifunc {
                R_X86_64_PC32
            } else {
                R_X86_64_PLT32
            };
            self.relocations.push(ElfRela {
                offset: code_off,
                sym_index: sym_idx,
                rel_type,
                addend: -4 + sym_off,
            });
        }
        for rel in std::mem::take(&mut self.pending_code_rels) {
            let Some((sym_idx, sym_off)) = self.reloc_sym_index(&rel.symbol, "pending code reloc")
            else {
                continue;
            };
            self.relocations.push(ElfRela {
                offset: rel.offset,
                sym_index: sym_idx,
                rel_type: rel.rel_type,
                addend: rel.addend + sym_off,
            });
        }

        // 解析函数地址重定位:
        // - LEA 指令 (instrument hook): 直接计算函数地址 → PC32
        // - MOV 指令 (外部函数 GOT 间接加载): GOTPCREL
        for (code_off, func_name, is_lea) in std::mem::take(&mut self.pending_func_addr_rels) {
            let site = if is_lea { "func addr PC32" } else { "func addr GOTPCREL" };
            let Some((sym_idx, sym_off)) = self.reloc_sym_index(&func_name, site) else {
                continue;
            };
            let rel_type = if is_lea { R_X86_64_PC32 } else { R_X86_64_GOTPCREL };
            self.relocations.push(ElfRela {
                offset: code_off,
                sym_index: sym_idx,
                rel_type,
                addend: -4 + sym_off,
            });
        }

        // 解析全局变量重定位 (使用最终符号表)
        // 数据符号统一使用 R_X86_64_PC32 (RIP-relative), 不经过 GOT.
        // TLS 变量也使用 PC32: 当前指令编码为 RIP-relative,
        // host linker 会正确解析 .tdata/.tbss 节内的 PC-relative 偏移.
        for (disp32_pos, var_name) in std::mem::take(&mut self.pending_data_rels) {
            // 内联 asm 的内存操作数可以写成 `sym±N(%rip)`（`arch/x86/include/asm/cpufeature.h`
            // 一类把 `%c[..]` 代入后就成了 `boot_cpu_data+114(%rip)`）。以前这种整串
            // 被当成一个符号名，链接期就是 `undefined reference to 'boot_cpu_data+114'`。
            // 剥位移现在由 [`reloc_sym_index`] 统一做（所有出口只剩一条路径），这里拿到的
            // `sym_off` 直接并进 PC32 的 addend（与下面 `_with_off` 同构：addend = off - 4）。
            // 不选解析侧：解析侧一旦拆了名字，那十来个 `SymDisp(sym, _)` 发射点全部
            // 忽略第二个字段 ⇒ 偏移会被悄悄丢掉（比报错更坏）。
            let Some((sym_idx, off)) = self.reloc_sym_index(&var_name, "global var PC32") else {
                continue;
            };
            self.relocations.push(ElfRela {
                offset: disp32_pos,
                sym_index: sym_idx,
                rel_type: R_X86_64_PC32,
                addend: off - 4,
            });
        }

        // 带元素偏移的全局重定位 (数组型 static local 逐元素初始化)
        // addend = elem_off - 4, 使 link-time 的 PC32 计算结果为
        // (sym + elem_off) - rip 地址.
        for (disp32_pos, var_name, elem_off) in std::mem::take(&mut self.pending_data_rels_with_off)
        {
            let Some((sym_idx, sym_off)) = self.reloc_sym_index(&var_name, "global var PC32 (elem)")
            else {
                continue;
            };
            self.relocations.push(ElfRela {
                offset: disp32_pos,
                sym_index: sym_idx,
                rel_type: R_X86_64_PC32,
                addend: elem_off as i64 - 4 + sym_off,
            });
        }

        // 解析 TLS 重定位 (relocatable mode)
        // 根据 tls_model 属性选择重定位类型:
        //   local-exec (默认) → R_X86_64_TPOFF32
        //   initial-exec → R_X86_64_TPOFF32 (host linker 会转换为 GOT 访问)
        //   local-dynamic/global-dynamic → R_X86_64_TPOFF32 (降级为 LE,
        //     GD/LD 需要运行时 TLS resolver 支持，内核不使用)
        for (disp32_pos, var_name) in std::mem::take(&mut self.pending_tls_rels) {
            let Some((sym_idx, sym_off)) = self.reloc_sym_index(&var_name, "TLS TPOFF32") else {
                continue;
            };
            let model = self
                .tls_var_models
                .get(&var_name)
                .map(|s| s.as_str())
                .unwrap_or("local-exec");
            let rel_type = match model {
                "initial-exec" | "local-exec" => R_X86_64_TPOFF32,
                // GD/LD 降级为 LE: 内核不使用动态 TLS resolver
                _ => R_X86_64_TPOFF32,
            };
            self.relocations.push(ElfRela {
                offset: disp32_pos,
                sym_index: sym_idx,
                rel_type,
                addend: sym_off,
            });
        }

        // 解析 SIB base=101 全局访问 (disp32 绝对寻址) - 需要 R_X86_64_32S
        for (disp32_pos, var_name) in std::mem::take(&mut self.pending_sib_data_rels) {
            let Some((sym_idx, sym_off)) = self.reloc_sym_index(&var_name, "global var ABS32S")
            else {
                continue;
            };
            self.relocations.push(ElfRela {
                offset: disp32_pos,
                sym_index: sym_idx,
                rel_type: R_X86_64_32S,
                addend: sym_off,
            });
        }

        // data 节重定位: rodata 字符串引用 (char *s = "hello") + 符号引用
        let mut custom_data_relas: std::collections::BTreeMap<usize, Vec<ElfRela>> =
            std::collections::BTreeMap::new();

        // 解析 data 节引用 rodata 字符串的重定位:
        // R_X86_64_64, 符号 = .rodata section, addend = 字符串 rodata 偏移
        for (data_off, str_off, custom_sec) in std::mem::take(&mut self.pending_data_rodata_rels) {
            let Some((sym_idx, _)) = self.reloc_sym_index(".rodata", "data->string literal R_64")
            else {
                continue;
            };
            let relocation = ElfRela {
                offset: data_off,
                sym_index: sym_idx,
                rel_type: R_X86_64_64,
                addend: str_off as i64,
            };
            if let Some(sec_idx) = custom_sec {
                custom_data_relas
                    .entry(sec_idx)
                    .or_default()
                    .push(relocation);
            } else {
                self.data_relocations.push(relocation);
            }
        }

        // P1-6: 解析 data 节 64 位符号引用重定位
        for (data_off, var_name, custom_sec, rel_type, addend) in
            std::mem::take(&mut self.pending_data64_rels)
        {
            let Some((sym_idx, sym_off)) =
                self.reloc_sym_index(&var_name, "data 64-bit symbol R_64")
            else {
                continue;
            };
            let relocation = ElfRela {
                offset: data_off,
                sym_index: sym_idx,
                rel_type,
                addend: addend + sym_off,
            };
            if let Some(sec_idx) = custom_sec {
                custom_data_relas
                    .entry(sec_idx)
                    .or_default()
                    .push(relocation);
            } else {
                self.data_relocations.push(relocation);
            }
        }
        // P1-7: data 节里指向本 TU **文本标号**（static local 跳转表 `&&label`）的
        // 64 位槽。`.Ll_*` 按 GAS 约定不进 symtab（`referenced_reloc_symbols` 排除
        // `.L` 前缀），所以符号取 `.text` **节符号**（st_value=0），addend 填标号在
        // 代码段内的偏移 —— 与 `pending_data_rodata_rels` 对字符串字面量同一手法。
        for (data_off, label_name, extra_addend) in
            std::mem::take(&mut self.pending_data_textlabel_rels)
        {
            let Some((sym_idx, _)) = self.reloc_sym_index(".text", "data->text label R_64") else {
                continue;
            };
            let Some(&label_off) = self.labels.get(&label_name) else {
                continue;
            };
            self.data_relocations.push(ElfRela {
                offset: data_off,
                sym_index: sym_idx,
                rel_type: R_X86_64_64,
                addend: label_off as i64 + extra_addend,
            });
        }
        for (sec_idx, relocations) in custom_data_relas {
            // `Some(usize::MAX)` 是「槽就在 .rodata 里」的哨兵，生产侧
            // （`push_data_symbol_rel` / `emit_string_ptr_slot`）已把它改道到
            // `pending_rodata_rels`；万一漏网，这里必须先拦住 —— 裸下标
            // `extra_sections[usize::MAX]` 是直接 panic（rc=134），
            // 而不是可诊断的编译错误。
            if sec_idx >= self.extra_sections.len() {
                self.report_internal_bug(
                    "E1104",
                    format!(
                        "data relocation targets section index {sec_idx}, but this TU has only {} \
                         extra section(s)",
                        self.extra_sections.len()
                    ),
                    "`Some(usize::MAX)` means \"inside .rodata\": such relocations must be \
                     emitted as `.rela.rodata` via pending_rodata_rels, not through the \
                     extra_sections index",
                );
                continue;
            }
            let name = format!(".rela{}", self.extra_sections[sec_idx].name);
            self.extra_rela_sections.push(ExtraRelaSection {
                name,
                relocations,
                target_sec_index: 7 + sec_idx as u32,
            });
        }

        // rodata 节内部重定位 (.rela.rodata): rodata 内 CompoundLit 结构体
        // 指针字段引用另一个 rodata 内的匿名 CompoundLit (C99 合法的文件地址常量)。
        // .rodata section header index = 2 (0=NULL, 1=.text, 2=.rodata, 3=.data, ...)
        if !self.pending_rodata_rels.is_empty() {
            let mut rodata_relas = Vec::new();
            for (off, sym_name, rel_type, addend) in std::mem::take(&mut self.pending_rodata_rels) {
                let Some((sym_idx, sym_off)) =
                    self.reloc_sym_index(&sym_name, ".rodata internal R_64")
                else {
                    continue;
                };
                rodata_relas.push(ElfRela {
                    offset: off,
                    sym_index: sym_idx,
                    rel_type,
                    addend: addend + sym_off,
                });
            }
            if !rodata_relas.is_empty() {
                self.extra_rela_sections.push(ExtraRelaSection {
                    name: ".rela.rodata".to_string(),
                    relocations: rodata_relas,
                    target_sec_index: 2, // .rodata
                });
            }
        }

        // __attribute__((constructor/destructor)): 生成 .init_array/.fini_array
        // 按 priority 排序: 小数字先执行 (constructor), 小数字后执行 (destructor)
        let mut constructors: Vec<(u16, String)> = vec![];
        let mut destructors: Vec<(u16, String)> = vec![];
        for decl in prog.decls.iter() {
            if let Decl::Func(f) = decl {
                if f.body.is_some() {
                    if f.is_constructor {
                        constructors.push((f.ctor_priority.unwrap_or(65535), f.name.name.clone()));
                    }
                    if f.is_destructor {
                        destructors.push((f.dtor_priority.unwrap_or(65535), f.name.name.clone()));
                    }
                }
            }
        }
        constructors.sort_by_key(|(p, _)| *p);
        destructors.sort_by_key(|(p, _)| *p);
        if !constructors.is_empty() || !destructors.is_empty() {
            let base_sec_idx = 7 + self.extra_sections.len() as u32;
            // .init_array
            if !constructors.is_empty() {
                let init_array_sec_idx = base_sec_idx;
                let mut init_data = Vec::with_capacity(constructors.len() * 8);
                let mut init_rels = Vec::with_capacity(constructors.len());
                for (_prio, func_name) in &constructors {
                    let entry_off = init_data.len() as u64;
                    init_data.extend_from_slice(&[0u8; 8]); // placeholder
                    let Some((sym_idx, _)) = self.reloc_sym_index(func_name, ".init_array entry")
                    else {
                        continue;
                    };
                    init_rels.push(ElfRela {
                        offset: entry_off,
                        sym_index: sym_idx,
                        rel_type: R_X86_64_64,
                        addend: 0,
                    });
                }
                self.extra_sections
                    .push(crate::codegen::shared::elf::ElfSection {
                        name: ".init_array".to_string(),
                        data: init_data,
                        ty: crate::codegen::shared::elf::SHT_PROGBITS,
                        flags: crate::codegen::shared::elf::SHF_ALLOC
                            | crate::codegen::shared::elf::SHF_WRITE,
                        align: 8,
                        group_name: None,
                    });
                self.extra_rela_sections.push(ExtraRelaSection {
                    name: ".rela.init_array".to_string(),
                    relocations: init_rels,
                    target_sec_index: init_array_sec_idx,
                });
            }
            // .fini_array
            if !destructors.is_empty() {
                let fini_array_sec_idx = base_sec_idx + if constructors.is_empty() { 0 } else { 1 };
                let mut fini_data = Vec::with_capacity(destructors.len() * 8);
                let mut fini_rels = Vec::with_capacity(destructors.len());
                for (_prio, func_name) in &destructors {
                    let entry_off = fini_data.len() as u64;
                    fini_data.extend_from_slice(&[0u8; 8]);
                    let Some((sym_idx, _)) = self.reloc_sym_index(func_name, ".fini_array entry")
                    else {
                        continue;
                    };
                    fini_rels.push(ElfRela {
                        offset: entry_off,
                        sym_index: sym_idx,
                        rel_type: R_X86_64_64,
                        addend: 0,
                    });
                }
                self.extra_sections
                    .push(crate::codegen::shared::elf::ElfSection {
                        name: ".fini_array".to_string(),
                        data: fini_data,
                        ty: crate::codegen::shared::elf::SHT_PROGBITS,
                        flags: crate::codegen::shared::elf::SHF_ALLOC
                            | crate::codegen::shared::elf::SHF_WRITE,
                        align: 8,
                        group_name: None,
                    });
                self.extra_rela_sections.push(ExtraRelaSection {
                    name: ".rela.fini_array".to_string(),
                    relocations: fini_rels,
                    target_sec_index: fini_array_sec_idx,
                });
            }
        }

        // 将源地址位于自定义函数中的重定位移入对应 .rela.<section>。
        let mut custom_relas: std::collections::BTreeMap<usize, Vec<ElfRela>> =
            std::collections::BTreeMap::new();
        let mut text_relas = vec![];
        // 待搬迁区间：自定义函数 + 可执行 pushsection 代码。字段含义与
        // `codegen.rs::compile_rel` 里那张表**同一个**：`(发射起点, 发射长度, 目标节下标,
        // 目标节内起点)`。以前这里用 `(start, end, ...)` 而那边用 `(start, size, ...)`，
        // 同一个四元组两套含义 —— `codegen.rs` 那份就是因为照这里错了一格（把
        // `func_start/emitted_start/emitted_size/sec_idx` 当 `start/size/sec_idx/sec_off` 用），
        // 跨节跳转被当成同节而写死了位移。含义只留一份，两边只能读成同一个东西。
        let move_ranges: Vec<(u64, u64, usize, u64)> = custom_func_ranges
            .iter()
            .map(|(_, emitted_start, emitted_size, sec_idx, sec_off)| {
                (
                    *emitted_start,
                    *emitted_size as u64,
                    *sec_idx as usize,
                    *sec_off,
                )
            })
            .chain(self.pushsection_code_ranges.iter().copied())
            .collect();
        for mut rel in self.relocations.drain(..) {
            // 区间**嵌套**：可执行 pushsection 的代码（`.altinstr_replacement` /
            // `.altinstr_aux`）是嵌在函数发射流里的，同一个重定位地址同时落在函数区间和
            // pushsection 区间内。必须取最内层（长度最小）那一层 —— 用 `.find` 取第一个
            // 会把 aux/replacement 的重定位全留进 `.rela.<函数节>` 的 NOP 洞里
            // （实测 map_kernel.o：`.rela.init.text` 里 offset 0x1e2/0x1e8/0x1ef/0x1f4
            // 四条本该属于 `.rela.altinstr_replacement` / `.rela.altinstr_aux`，于是
            // objtool 在 aux 里看到 `jmp +0`、在 replacement 里看到 `jmp +5`，报
            // `can't find jump dest instruction at .altinstr_replacement+0x5`）。
            if let Some((emitted_start, _emitted_size, sec_idx, sec_off)) = move_ranges
                .iter()
                .filter(|(s, sz, _, _)| rel.offset >= *s && rel.offset < *s + *sz)
                .min_by_key(|(_, sz, _, _)| *sz)
                .copied()
            {
                rel.offset = sec_off + (rel.offset - emitted_start);
                custom_relas.entry(sec_idx).or_default().push(rel);
            } else {
                text_relas.push(rel);
            }
        }
        self.relocations = text_relas;
        for (sec_idx, relocations) in custom_relas {
            if relocations.is_empty() {
                continue;
            }
            let rela_name = format!(".rela{}", self.extra_sections[sec_idx].name);
            let target_sec_index = 7 + sec_idx as u32;
            if let Some(existing) = self
                .extra_rela_sections
                .iter_mut()
                .find(|r| r.target_sec_index == target_sec_index)
            {
                existing.relocations.extend(relocations);
            } else {
                self.extra_rela_sections.push(ExtraRelaSection {
                    name: rela_name,
                    relocations,
                    target_sec_index,
                });
            }
        }

        // 解析内部跳转 (本地标签) 已全部完成 (patch_local_jumps 在 Phase 2
        // 前), 现在把最终机器码覆盖到自定义节副本并将 .text 原位置 NOP。
        // (自 monolith compile_rel 尾部恢复: 缺失时 .fixup 等可执行
        // pushsection 节只有对齐填充, 真实代码留在 .text。)
        // 顺序不能反：可执行 pushsection（`.altinstr_replacement`/`.fixup`）的代码是
        // **嵌在函数发射流里**的（open 时就在 self.code 当前位置继续发），它天然落在
        // 某个 `custom_func_ranges` 区间内部。先搬函数会把整段（连同 replacement 的
        // 真实字节）复制进目标节、再把原位填 0x90；随后 pushsection 段看到「已全 0x90」
        // 直接 continue，目标节里只剩对齐垫出来的 8 个 0x90 —— 实测 `pvclock.o` 的
        // `.altinstr_replacement` 就是这样丢掉了 `lfence; rdtsc`/`rdtscp`，objtool 报
        // `weirdly overlapping alternative!`。先搬 pushsection 再搬函数，函数副本里
        // 只留下 0x90 洞（与 GAS 把 replacement 排除在函数外的效果等价）。
        let pushsection_ranges = std::mem::take(&mut self.pushsection_code_ranges);
        for (emitted_start, emitted_size, sec_idx, sec_off) in &pushsection_ranges {
            let src_start = *emitted_start as usize;
            let src_end = (*emitted_start + *emitted_size) as usize;
            // Skip if already moved (parallel codegen: chunk's compile_rel
            // already moved code to extra_sections, leaving NOPs in .text)
            let src = &self.code[src_start..src_end];
            if src.iter().all(|&b| b == 0x90) {
                continue;
            }
            let src = src.to_vec();
            let dst_start = *sec_off as usize;
            let dst_end = dst_start + *emitted_size as usize;
            self.extra_sections[*sec_idx].data[dst_start..dst_end].copy_from_slice(&src);
            self.code[src_start..src_end].fill(0x90);
        }
        self.pushsection_code_ranges = pushsection_ranges;
        for (_, emitted_start, emitted_size, sec_idx, sec_off) in custom_func_ranges {
            let src_start = *emitted_start as usize;
            let src_end = (*emitted_start + *emitted_size as u64) as usize;
            // Skip if already moved (parallel codegen: chunk already placed
            // correct code in extra_sections, leaving NOPs in .text)
            let src = &self.code[src_start..src_end];
            if src.iter().all(|&b| b == 0x90) {
                continue;
            }
            let src = src.to_vec();
            let dst_start = *sec_off as usize;
            let dst_end = dst_start + *emitted_size;
            self.extra_sections[*sec_idx as usize].data[dst_start..dst_end].copy_from_slice(&src);
            self.code[src_start..src_end].fill(0x90);
        }
        // 代码已入各自定义节: 捕获函数最终字节供 -S 输出分组 (monolith 同位)
        self.finalize_func_asm_layout(prog, custom_func_ranges);
    }
}

/// 把 `sym+16` / `sym-8` / `sym + 0x10` 这种「符号名 + 尾部常数偏移」拆成
/// `(符号名, 偏移)`。没有尾部偏移时原样返回 `(s, 0)`。
///
/// 为什么放在重定位发射的出口而不是解析侧：内联 asm 的内存操作数文本（`SymDisp` 的符号部分）
/// 会直接当名字存进 `pending_data_rels`，而下游那十来个 `SymDisp(sym, _)` 发射点全部
/// **忽略**偏移字段；在解析侧拆名字就等于默默丢偏移。出口这里只有一个，拿得到 addend。
pub(crate) fn split_sym_offset(name: &str) -> (&str, i32) {
    // 【`N+sym` 形态先判】下面那个循环从右往左剥的是 `sym±N`；per-CPU 成员访问那条路给出的
    // 是**反过来**的 `N+sym`（实测 `arch/x86/kernel/kvm.o` 的
    // `__raw_callee_save___kvm_vcpu_is_preempted` 报
    // `undefined reference to '16+steal_time'`，而同一个对象里 `steal_time` 本身是有定义的
    // `D` 符号）。GAS 两种写法都接受，所以这里也必须剥——而且 ELF 符号名不以数字开头，
    // 「以数字开头」本身就说明这是一次表达式而不是一个名字。
    // 必须放在循环**之前**：循环里那个「尾巴不是纯数字就当不是偏移形状」的早退会直接
    // 把 `16+steal_time` 原样退回，永远轮不到后面的分支。
    // 偏移仍然交回调用方并进 addend（与下面同构）：发射侧那几个 `SymDisp(sym, _)` 不看
    // 第二个字段，所以拆分只能做在这里（见 `pending_data_rels` 那条注释）。
    if let Some(pos) = name.find(['+', '-']) {
        if pos > 0 {
            if let Some(v) = asm_number(&name[..pos]) {
                let tail = name[pos + 1..].trim();
                if !tail.is_empty() {
                    let signed = if name.as_bytes()[pos] == b'-' { -v } else { v };
                    if let Ok(o) = i32::try_from(signed) {
                        return (tail, o);
                    }
                }
            }
        }
    }
    // 从右往左找第一个不在括号里的 '+'/'-'，右边必须是一串纯数字（十或十六进制），
    // 否则不是本形状（例如 C++ 模板名、带 `.` 的节名都不该被动）。
    let bytes = name.as_bytes();
    let mut depth = 0i32;
    let mut i = bytes.len();
    while i > 0 {
        let prev = i - 1;
        match bytes[prev] {
            b')' => depth += 1,
            b'(' => depth -= 1,
            b'+' | b'-' if depth == 0 => {
                let tail = name[prev + 1..].trim();
                let value = asm_number(tail);
                if let Some(v) = value {
                    if prev == 0 {
                        // 整个名字就是个数：不当偏移处理
                        return (name, 0);
                    }
                    let base = name[..prev].trim_end();
                    let signed = if bytes[prev] == b'-' { -v } else { v };
                    // 偏移超出 i32 就不是能发 PC32 的形状，原样交给下面报错
                    if let Ok(o) = i32::try_from(signed) {
                        if !base.is_empty() {
                            return (base, o);
                        }
                    }
                    return (name, 0);
                }
                return (name, 0);
            },
            _ => {},
        }
        i = prev;
    }
    (name, 0)
}

/// 表达式原子的数值解析：十进制或 `0x` 十六进制；不是纯数字就 `None`。
/// `sym±N` 与 `N+sym` 两条形态共用它（同一个判据只写一处）。
fn asm_number(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).ok();
    }
    t.parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Abi, Arch, Os, TargetTriple};
    use crate::diagnostics::Severity;
    use crate::target::Target;

    fn x86() -> Codegen {
        Codegen::new(Target::resolve(&TargetTriple {
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: Os::Linux,
            abi: Abi::Gnu,
            x86_bits: TargetTriple::NATIVE_BITS,
        }))
    }

    fn error_codes(cg: &Codegen) -> Vec<&'static str> {
        cg.diagnostics
            .iter()
            .filter(|d| d.sev == Severity::Error)
            .map(|d| d.code)
            .collect()
    }

    /// 缺陷 D 主体: 符号缺失不得再静默变成 `STN_UNDEF` (index 0)。
    /// index 0 在链接期按 ELF 规格取 `S = 0`, 重定位静默落到镜像起点。
    #[test]
    fn missing_symbol_reports_e1103_instead_of_yielding_index_zero() {
        let mut cg = x86();
        assert_eq!(cg.reloc_sym_index("printf", "call PLT32/PC32"), None);
        assert_eq!(error_codes(&cg), vec!["E1103"]);
        assert!(cg.diagnostics[0].msg.contains("printf"));
    }

    #[test]
    fn known_symbol_resolves_without_diagnostic() {
        let mut cg = x86();
        cg.sym_index.insert("g".to_string(), 7);
        assert_eq!(cg.reloc_sym_index("g", "global var PC32"), Some((7, 0)));
        assert!(cg.diagnostics.is_empty(), "unexpected: {:?}", error_codes(&cg));
    }

    /// 单一真源的另一半: `asm_label_map` 归一化必须在唯一的解析路径里做。
    /// 旧代码 9 个站点中 3 个没做, 导致同一语义在不同重定位类型下结果不同。
    #[test]
    fn asm_label_names_are_normalized_before_lookup() {
        let mut cg = x86();
        cg.asm_label_map
            .insert("c_name".to_string(), "asm_name".to_string());
        cg.sym_index.insert("asm_name".to_string(), 3);
        assert_eq!(cg.reloc_sym_index("c_name", "call PLT32/PC32"), Some((3, 0)));
        assert!(cg.diagnostics.is_empty(), "unexpected: {:?}", error_codes(&cg));
    }

    /// 编译器自造的 `.L` 标号不进 symtab, 链接期根本不可能解析它。
    /// 走到这条路径只能说明发射器漏了回填, 诊断文案必须分清这两类。
    #[test]
    fn unresolved_internal_label_is_worded_as_an_emitter_bug() {
        let mut cg = x86();
        assert_eq!(cg.reloc_sym_index(".L5", "pending code reloc"), None);
        assert_eq!(error_codes(&cg), vec!["E1103"]);
        let msg = &cg.diagnostics[0].msg;
        assert!(msg.contains(".L5") && msg.contains("internal label"), "msg = {msg}");
    }

    /// 节符号只在对应节非空时才创建, 所以不能统一预解析。这里验证
    /// 「节非空 → 符号存在」这一正常局面不会误报。
    #[test]
    fn section_symbols_resolve_when_their_section_is_present() {
        let mut cg = x86();
        cg.sym_index.insert(".rodata".to_string(), 2);
        cg.sym_index.insert(".data".to_string(), 4);
        assert_eq!(cg.reloc_sym_index(".rodata", "string literal PC32"), Some((2, 0)));
        assert_eq!(
            cg.reloc_sym_index(".data", "ubsan OutOfBoundsData PC32"),
            Some((4, 0))
        );
        assert!(cg.diagnostics.is_empty(), "unexpected: {:?}", error_codes(&cg));
    }

    /// `sym±N`（内联 asm 的位移写法）必须在这条**唯一解析路径**上拆出位移。
    /// 以前只有两条出口自己拆过一次，漏掉的那几条出口就是全树 417 处
    /// `undefined reference to 'boot_cpu_data+114'`（站点 `.altinstr_aux+0x2`）。
    /// 基名也查不到时仍得报错（不能静默给 0），且报的是原始整串。
    #[test]
    fn trailing_symbol_offset_is_stripped_and_returned_as_addend_delta() {
        let mut cg = x86();
        cg.sym_index.insert("boot_cpu_data".to_string(), 11);
        assert_eq!(
            cg.reloc_sym_index("boot_cpu_data+114", "global var PC32"),
            Some((11, 114))
        );
        assert_eq!(
            cg.reloc_sym_index("boot_cpu_data-0x10", "pending code reloc"),
            Some((11, -16))
        );
        // 整名命中优先：即使 symtab 里真有个带位移的名字也不该被拆。
        cg.sym_index.insert("weird+8".to_string(), 12);
        assert_eq!(cg.reloc_sym_index("weird+8", "data 64-bit symbol R_64"), Some((12, 0)));
        assert_eq!(cg.reloc_sym_index("boot_cpu_data", "global var PC32"), Some((11, 0)));
        assert!(cg.diagnostics.is_empty(), "unexpected: {:?}", error_codes(&cg));

        let mut cg2 = x86();
        assert_eq!(cg2.reloc_sym_index("nope+8", "global var PC32"), None);
        assert_eq!(error_codes(&cg2), vec!["E1103"]);
        assert!(cg2.diagnostics[0].msg.contains("nope+8"), "msg = {}", cg2.diagnostics[0].msg);
    }
}

/// `split_sym_offset` 的两种形态：`sym±N`（`cpufeature.h` 的 `boot_cpu_data+114`）与
/// `N+sym`（per-CPU 成员访问，实测 `arch/x86/kernel/kvm.o` 的 `16+steal_time`）。
/// 反向对照同重要：C++ 模板名/带 `-` 的节名都不该被拆，拆错会把一个本来存在的符号
/// 变成两个都不存在的碎片。
#[test]
fn split_sym_offset_handles_both_operand_orders() {
    // `sym+N` / `sym-N`
    assert_eq!(split_sym_offset("boot_cpu_data+114"), ("boot_cpu_data", 114));
    assert_eq!(split_sym_offset("steal_time+16"), ("steal_time", 16));
    assert_eq!(split_sym_offset("sym-8"), ("sym", -8));
    assert_eq!(split_sym_offset("sym+0x10"), ("sym", 16));
    // `N+sym`：per-CPU 那条路给出的反序写法
    assert_eq!(split_sym_offset("16+steal_time"), ("steal_time", 16));
    assert_eq!(split_sym_offset("0x10+per_cpu_base"), ("per_cpu_base", 16));
    // 不该动的形状
    assert_eq!(split_sym_offset("plain_symbol"), ("plain_symbol", 0));
    assert_eq!(split_sym_offset("_ZL3fuf"), ("_ZL3fuf", 0));
    // 整个名字就是个数：不当偏移处理（原有语义，别退化成 ("", 42)）
    assert_eq!(split_sym_offset("42"), ("42", 0));
    // 括号里的加号不参与，而名字主体也不是「数字打头」的表达式 ⇒ 整串原样退回
    // （循环里那个「尾巴不是纯数字」的早退就是为这类形状准备的）。
    assert_eq!(split_sym_offset("mod+1(bar+2)"), ("mod+1(bar+2)", 0));
    // 尾巴不是纯数字 ⇒ 不是偏移形状
    assert_eq!(split_sym_offset("a+ b"), ("a+ b", 0));
}
