// src/codegen/x86_64/elf/resolve.rs
// ET_EXEC/ET_REL final symbol resolution and relocation patching.

use crate::ast::{Decl, Program};
use crate::codegen::shared::elf::{
    ElfRela, ExtraRelaSection, R_X86_64_32S, R_X86_64_64, R_X86_64_GOTPCREL, R_X86_64_PC32,
    R_X86_64_PLT32, R_X86_64_TPOFF32, STB_LOCAL,
};
use crate::codegen::x86_64::{Codegen, PendingCodeRel};

impl Codegen {
    pub fn resolve(&mut self, rodata_start: u64, data_start: u64) {
        for (p, name) in &self.pending_data_rels {
            self.disasm_data.insert(*p, name.clone());
        }
        // 修补跳转；ET_REL 下最终仍未解析的符号降级为 PC32 relocation。
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            let Some(&dst) = self.labels.get(&p.target) else {
                if self.relocatable && !p.is_push_imm {
                    let rel_off = p.disp_offset;
                    self.pending_code_rels.push(PendingCodeRel {
                        offset: rel_off,
                        symbol: p.target,
                        rel_type: R_X86_64_PC32,
                        addend: -4,
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
                // push $label: imm32 = absolute address (lower 32 bits)
                self.code[off..off + 4].copy_from_slice(&(dst as i32).to_le_bytes());
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
        for rel in self.pending_custom_sec_rels.drain(..) {
            let sym_idx = self.sym_index.get(&rel.symbol).copied().unwrap_or(0);
            let abs_offset = rel.offset;
            let rela = ElfRela {
                offset: abs_offset,
                sym_index: sym_idx,
                rel_type: rel.rel_type,
                addend: rel.addend,
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
        // 查找 .rodata section 符号索引
        let rodata_sym_idx = self.sym_index.get(".rodata").copied();

        // Patch __mcount_loc relocations with .text section symbol index
        let text_sym_idx = self.sym_index.get(".text").copied().unwrap_or(1);
        for rela_sec in &mut self.extra_rela_sections {
            if rela_sec.name == ".rela__mcount_loc" {
                for rel in &mut rela_sec.relocations {
                    rel.sym_index = text_sym_idx;
                }
            }
        }

        for (code_pos, data) in self.string_refs.drain(..) {
            let str_off = self.string_pool[&data] as i64;
            self.relocations.push(ElfRela {
                offset: code_pos + 3,
                sym_index: rodata_sym_idx.unwrap_or(0),
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
        for (code_pos, rodata_off) in self.float_refs.drain(..) {
            // movsd/movss xmm, [RIP+disp32] 是 8 字节指令, disp32 在 +4。
            // ELF64 R_X86_64_PC32: disp32 = S + A - P, 其中 P 是
            // 重定位字段地址 code_pos + 4。CPU 运行时从下一条 RIP
            // (code_pos + 8) 加 disp32, 有效地址就是 S + A + 4。
            // 目标地址为 .rodata + rodata_off, 因此 A = rodata_off - 4。
            self.relocations.push(ElfRela {
                offset: code_pos + 4,
                sym_index: rodata_sym_idx.unwrap_or(0),
                rel_type: R_X86_64_PC32,
                addend: rodata_off as i64 - 4,
            });
        }
        // x87 fld 字面量重定位 (6 字节指令, disp32 @ byte 2)
        for (code_pos, rodata_off) in self.x87_float_refs.drain(..) {
            self.relocations.push(ElfRela {
                offset: code_pos + 2,
                sym_index: rodata_sym_idx.unwrap_or(0),
                rel_type: R_X86_64_PC32,
                // fld 6 字节, disp32 在 +2, 下一条 RIP 是 code_pos + 6。
                // P = code_pos + 2, 所以有效地址仍是 S + A + 4。
                addend: rodata_off as i64 - 4,
            });
        }

        // -fbounds-check ubsan 诊断数据重定位:
        // LEA rdi, [rip+disp32] → PC32 重定位 (指向 .data 内 OutOfBoundsData;
        // libubsan SourceLocation::acquire() 原子写 Column, .rodata 只读会段错误)
        let data_sym_idx = self.sym_index.get(".data").copied();
        for (code_pos, data_off) in self.ubsan_lea_refs.drain(..) {
            self.relocations.push(ElfRela {
                offset: code_pos + 3,
                sym_index: data_sym_idx.unwrap_or(0),
                rel_type: R_X86_64_PC32,
                // LEA 7 bytes, disp32 at byte 3.
                // effective addr = S + A + 4, A = data_off - 4.
                addend: data_off as i64 - 4,
            });
        }
        // data 内绝对指针 (Filename@0, &ArrayType@16, &IndexType@24)
        // → R_X86_64_64 指向 .rodata 内 TypeDescriptor / 文件名字符串
        for (data_off, field_off, target_off) in self.ubsan_ptr_refs.drain(..) {
            self.data_relocations.push(ElfRela {
                offset: data_off + field_off,
                sym_index: rodata_sym_idx.unwrap_or(0),
                rel_type: R_X86_64_64,
                addend: target_off as i64,
            });
        }

        // 解析函数调用重定位 (本 TU 内的 static/本地函数用 PC32,
        // 跨 TU 外部调用用 PLT32 以兼容 PIE/动态链接).
        for (code_off, func_name) in self.pending_func_rels.drain(..) {
            let resolved = self
                .asm_label_map
                .get(&func_name)
                .cloned()
                .unwrap_or(func_name);
            let sym_idx = self.sym_index.get(&resolved).copied().unwrap_or(0);
            // 检查符号绑定: LOCAL 表示本 TU 定义 (含 static), 用 PC32
            // 产生位置无关重定位, 避免 PLT 跳转
            let is_local = self
                .symbols
                .get(sym_idx as usize)
                .map(|s| s.bind == STB_LOCAL)
                .unwrap_or(false);
            let rel_type = if is_local { R_X86_64_PC32 } else { R_X86_64_PLT32 };
            self.relocations.push(ElfRela {
                offset: code_off,
                sym_index: sym_idx,
                rel_type,
                addend: -4,
            });
        }
        for rel in self.pending_code_rels.drain(..) {
            let sym_idx = self.sym_index.get(&rel.symbol).copied().unwrap_or(0);
            self.relocations.push(ElfRela {
                offset: rel.offset,
                sym_index: sym_idx,
                rel_type: rel.rel_type,
                addend: rel.addend,
            });
        }

        // 解析函数地址重定位:
        // - LEA 指令 (instrument hook): 直接计算函数地址 → PC32
        // - MOV 指令 (外部函数 GOT 间接加载): GOTPCREL
        for (code_off, func_name, is_lea) in self.pending_func_addr_rels.drain(..) {
            let resolved = self
                .asm_label_map
                .get(&func_name)
                .cloned()
                .unwrap_or(func_name);
            let sym_idx = self.sym_index.get(&resolved).copied().unwrap_or(0);
            let rel_type = if is_lea { R_X86_64_PC32 } else { R_X86_64_GOTPCREL };
            self.relocations.push(ElfRela {
                offset: code_off,
                sym_index: sym_idx,
                rel_type,
                addend: -4,
            });
        }

        // 解析全局变量重定位 (使用最终符号表)
        // 数据符号统一使用 R_X86_64_PC32 (RIP-relative), 不经过 GOT.
        // TLS 变量也使用 PC32: 当前指令编码为 RIP-relative,
        // host linker 会正确解析 .tdata/.tbss 节内的 PC-relative 偏移.
        for (disp32_pos, var_name) in self.pending_data_rels.drain(..) {
            let resolved = self
                .asm_label_map
                .get(&var_name)
                .cloned()
                .unwrap_or(var_name);
            let sym_idx = self.sym_index.get(&resolved).copied().unwrap_or(0);
            self.relocations.push(ElfRela {
                offset: disp32_pos,
                sym_index: sym_idx,
                rel_type: R_X86_64_PC32,
                addend: -4,
            });
        }

        // 解析 TLS 重定位 (relocatable mode)
        // 根据 tls_model 属性选择重定位类型:
        //   local-exec (默认) → R_X86_64_TPOFF32
        //   initial-exec → R_X86_64_TPOFF32 (host linker 会转换为 GOT 访问)
        //   local-dynamic/global-dynamic → R_X86_64_TPOFF32 (降级为 LE,
        //     GD/LD 需要运行时 TLS resolver 支持，内核不使用)
        for (disp32_pos, var_name) in self.pending_tls_rels.drain(..) {
            let model = self
                .tls_var_models
                .get(&var_name)
                .map(|s| s.as_str())
                .unwrap_or("local-exec");
            let resolved = self
                .asm_label_map
                .get(&var_name)
                .cloned()
                .unwrap_or(var_name);
            let sym_idx = self.sym_index.get(&resolved).copied().unwrap_or(0);
            let rel_type = match model {
                "initial-exec" | "local-exec" => R_X86_64_TPOFF32,
                // GD/LD 降级为 LE: 内核不使用动态 TLS resolver
                _ => R_X86_64_TPOFF32,
            };
            self.relocations.push(ElfRela {
                offset: disp32_pos,
                sym_index: sym_idx,
                rel_type,
                addend: 0,
            });
        }

        // 解析 SIB base=101 全局访问 (disp32 绝对寻址) - 需要 R_X86_64_32S
        for (disp32_pos, var_name) in self.pending_sib_data_rels.drain(..) {
            let resolved = self
                .asm_label_map
                .get(&var_name)
                .cloned()
                .unwrap_or(var_name);
            let sym_idx = self.sym_index.get(&resolved).copied().unwrap_or(0);
            self.relocations.push(ElfRela {
                offset: disp32_pos,
                sym_index: sym_idx,
                rel_type: R_X86_64_32S,
                addend: 0,
            });
        }

        // data 节重定位: rodata 字符串引用 (char *s = "hello") + 符号引用
        let mut custom_data_relas: std::collections::BTreeMap<usize, Vec<ElfRela>> =
            std::collections::BTreeMap::new();

        // 解析 data 节引用 rodata 字符串的重定位:
        // R_X86_64_64, 符号 = .rodata section, addend = 字符串 rodata 偏移
        for (data_off, str_off, custom_sec) in self.pending_data_rodata_rels.drain(..) {
            let relocation = ElfRela {
                offset: data_off,
                sym_index: rodata_sym_idx.unwrap_or(0),
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
        for (data_off, var_name, custom_sec, rel_type, addend) in self.pending_data64_rels.drain(..)
        {
            let resolved = self
                .asm_label_map
                .get(&var_name)
                .cloned()
                .unwrap_or(var_name);
            let sym_idx = self.sym_index.get(&resolved).copied().unwrap_or(0);
            let relocation = ElfRela {
                offset: data_off,
                sym_index: sym_idx,
                rel_type,
                addend,
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
        for (sec_idx, relocations) in custom_data_relas {
            self.extra_rela_sections.push(ExtraRelaSection {
                name: format!(".rela{}", self.extra_sections[sec_idx].name),
                relocations,
                target_sec_index: 7 + sec_idx as u32,
            });
        }

        // rodata 节内部重定位 (.rela.rodata): rodata 内 CompoundLit 结构体
        // 指针字段引用另一个 rodata 内的匿名 CompoundLit (C99 合法的文件地址常量)。
        // .rodata section header index = 2 (0=NULL, 1=.text, 2=.rodata, 3=.data, ...)
        if !self.pending_rodata_rels.is_empty() {
            let mut rodata_relas = Vec::new();
            for (off, sym_name, rel_type, addend) in self.pending_rodata_rels.drain(..) {
                let resolved = self
                    .asm_label_map
                    .get(&sym_name)
                    .cloned()
                    .unwrap_or(sym_name);
                // 符号可能还未进入 sym_index, 兑底 0; 实际符号名会在最终
                // symtab 里 (compile_rel 尾部扫 globals_raw)。
                let sym_idx = self.sym_index.get(&resolved).copied().unwrap_or(0);
                rodata_relas.push(ElfRela {
                    offset: off,
                    sym_index: sym_idx,
                    rel_type,
                    addend,
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
                    let sym_idx = self.sym_index.get(func_name).copied().unwrap_or(0);
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
                    let sym_idx = self.sym_index.get(func_name).copied().unwrap_or(0);
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
        // 待搬迁区间: 自定义函数 + 可执行 pushsection 代码 (text start, end, sec_idx, sec_off)
        let move_ranges: Vec<(u64, u64, usize, u64)> = custom_func_ranges
            .iter()
            .map(|(_, emitted_start, emitted_size, sec_idx, sec_off)| {
                (
                    *emitted_start,
                    *emitted_start + *emitted_size as u64,
                    *sec_idx as usize,
                    *sec_off,
                )
            })
            .chain(
                self.pushsection_code_ranges
                    .iter()
                    .map(|(s, sz, i, o)| (*s, *s + *sz, (*i), *o)),
            )
            .collect();
        for mut rel in self.relocations.drain(..) {
            if let Some((emitted_start, _emitted_end, sec_idx, sec_off)) = move_ranges
                .iter()
                .find(|(s, e, _, _)| rel.offset >= *s && rel.offset < *e)
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
        // 可执行 pushsection (.fixup/.altinstr_replacement) 代码区间同样搬迁
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
        // 代码已入各自定义节: 捕获函数最终字节供 -S 输出分组 (monolith 同位)
        self.finalize_func_asm_layout(prog, custom_func_ranges);
    }
}
