// src/codegen/x86_64/elf/rel_symbols.rs
//
// ET_REL 符号表构建 (Phase 2)。自 monolith (git 28a9063) `compile_rel` 的
// "Phase 2: 收集所有符号" 段逐字恢复：模块化迁移 (12bc4ff) 时该段被降级为
// 仅复用 exec 路径的 `collect_exec_symbols`, 丢失了 asm 函数符号、alias /
// target_clones 变体、嵌套函数、__mcount_loc、pushsection 数字标签符号、
// section 符号与 implicit UND 等条目, 造成 symtab 相关集成测试成片失败。
// 仅移动/可见性调整, 不改语义 (backend_refactor_plan §3.1 原则 1)。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf::{
    ElfRela, ElfSection, ElfSym, ExtraRelaSection, R_X86_64_64, R_X86_64_PC32, SHF_ALLOC,
    SHN_UNDEF, SHT_PROGBITS, STB_GLOBAL, STB_LOCAL, STB_WEAK, STT_FUNC, STT_GNU_IFUNC, STT_NOTYPE,
    STT_OBJECT, STT_SECTION, STT_TLS, STV_DEFAULT,
};
use crate::codegen::x86_64::{Codegen, GlobalVar, PendingCodeRel};
use std::collections::HashMap;

impl Codegen {
    /// Phase 2: 收集所有符号 (正确顺序: STN_UNDEF → LOCAL → GLOBAL)
    /// 先收集到临时 vec, 然后排序构建最终 symtab
    pub(crate) fn collect_rel_symbols(
        &mut self,
        prog: &Program,
        custom_func_offsets: &HashMap<String, (u16, u64)>,
    ) {
        let mut local_syms: Vec<ElfSym> = vec![];
        let mut global_syms: Vec<ElfSym> = vec![];

        // 全局变量符号（extern 声明 + 同 TU 定义会重复出现在 prog.decls）
        let mut emitted_var_syms: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let var_defined: std::collections::HashSet<String> = prog
            .decls
            .iter()
            .filter_map(|d| match d {
                Decl::Var(v) if !v.is_extern || v.init.is_some() => Some(v.name.name.clone()),
                _ => None,
            })
            .collect();
        let var_has_definition = |name: &str| -> bool { var_defined.contains(name) };
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                if v.register_name.is_some() {
                    continue;
                }
                // __attribute__((alias)): 变量别名由 alias 路径处理
                if v.alias_name.is_some() {
                    continue;
                }
                // Skip variables with empty names
                if v.name.name.is_empty() {
                    continue;
                }
                // 超大数组 (>1GB) 未分配存储，跳过符号发射
                if v.ty.size() > (1 << 30) {
                    continue;
                }
                if v.is_extern && v.init.is_none() && var_has_definition(&v.name.name) {
                    continue;
                }
                if !emitted_var_syms.insert(v.name.name.clone()) {
                    continue;
                }
                let g = self.globals_raw[&v.name.name].clone();
                let (section, bind) = if v.is_extern && v.init.is_none() && g.section == SHN_UNDEF {
                    (SHN_UNDEF, if v.is_weak { STB_WEAK } else { STB_GLOBAL })
                } else {
                    (
                        g.section,
                        if v.is_weak {
                            STB_WEAK
                        } else if v.is_static {
                            STB_LOCAL
                        } else {
                            STB_GLOBAL
                        },
                    )
                };
                let sym = ElfSym {
                    name: self.resolve_asm_label(&v.name.name).to_string(),
                    section,
                    value: g.offset,
                    size: g.size as u64,
                    bind,
                    ty: if g.is_tls { STT_TLS } else { STT_OBJECT },
                    visibility: Self::parse_visibility(&v.visibility),
                };
                if v.is_static {
                    local_syms.push(sym);
                } else {
                    global_syms.push(sym);
                }
            }
        }

        // Static 局部变量符号: 在 self.globals_raw 中但不在 prog.decls 中
        //
        // **不得直接迭代 HashMap**: `globals_raw` 是 `HashMap`，迭代序逐进程随机，
        // 会直接落入 `.symtab`/`.strtab` 条目序与 `.rela.text` 的 `r_info` 索引。
        // 按 (布局偏移, 名字) 排: 偏移序即符号在 .data/.bss 里的实际布局序。
        let mut globals_in_layout_order: Vec<(&String, &GlobalVar)> =
            self.globals_raw.iter().collect();
        globals_in_layout_order
            .sort_by(|a, b| a.1.offset.cmp(&b.1.offset).then_with(|| a.0.cmp(b.0)));
        for (name, g) in globals_in_layout_order {
            if emitted_var_syms.contains(name.as_str()) {
                continue;
            }
            if self.defined_funcs.contains(name.as_str()) {
                continue;
            }
            // 跳过空名和外部引用 (section=UNDEF)
            if name.is_empty() || g.section == SHN_UNDEF {
                continue;
            }
            let sym = ElfSym {
                name: name.clone(),
                section: g.section,
                value: g.offset,
                size: g.size as u64,
                bind: STB_LOCAL,
                ty: if g.is_tls { STT_TLS } else { STT_OBJECT },
                visibility: STV_DEFAULT,
            };
            local_syms.push(sym);
            emitted_var_syms.insert(name.clone());
        }

        // extern 函数声明 (无函数体 = 外部引用, 排除 alias 声明)
        // 若本 TU 内有同名函数定义 (含 asm label 匹配), 则跳过 UND 符号
        // 预计算有定义的函数名集合 (含 asm label 解析名), 避免 O(n²) 线性扫描.
        let func_defined: std::collections::HashSet<String> = prog
            .decls
            .iter()
            .filter_map(|d| match d {
                Decl::Func(f) if f.body.is_some() => Some(f.name.name.clone()),
                _ => None,
            })
            .collect();
        // 收集所有 asm label 解析名 (有定义的函数)
        let func_defined_asm: std::collections::HashSet<String> = prog
            .decls
            .iter()
            .filter_map(|d| match d {
                Decl::Func(f) if f.body.is_some() && f.asm_label.is_some() => {
                    Some(self.resolve_asm_label(&f.name.name).to_string())
                },
                _ => None,
            })
            .collect();
        let func_has_definition =
            |name: &str| -> bool { func_defined.contains(name) || func_defined_asm.contains(name) };
        let mut emitted_func_syms: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_none() && f.alias_name.is_none() {
                    // 检查 C 名和 asm label 解析后的名字
                    let resolved = self.resolve_asm_label(&f.name.name).to_string();
                    // Skip functions with empty names
                    if resolved.is_empty() {
                        continue;
                    }
                    let has_c = func_has_definition(&f.name.name);
                    let has_resolved = func_has_definition(&resolved);
                    if has_c || has_resolved {
                        continue;
                    }
                    if !emitted_func_syms.insert(f.name.name.clone()) {
                        continue;
                    }
                    global_syms.push(ElfSym {
                        name: self.resolve_asm_label(&f.name.name).to_string(),
                        section: SHN_UNDEF,
                        value: 0,
                        size: 0,
                        bind: if f.is_weak { STB_WEAK } else { STB_GLOBAL },
                        ty: STT_FUNC,
                        visibility: Self::parse_visibility(&f.visibility),
                    });
                }
            }
        }

        // Stack canary: 添加 __stack_chk_fail 未定义符号 (由 libc 提供)
        if self.target.stack_protector {
            global_syms.push(ElfSym {
                name: "__stack_chk_fail".to_string(),
                section: SHN_UNDEF,
                value: 0,
                size: 0,
                bind: STB_GLOBAL,
                ty: STT_FUNC,
                visibility: STV_DEFAULT,
            });
        }

        // ftrace: add mcount/__fentry__ undefined symbols
        match self.target.ftrace_mode {
            crate::cli::FtraceMode::MCount => {
                global_syms.push(ElfSym {
                    name: "mcount".to_string(),
                    section: SHN_UNDEF,
                    value: 0,
                    size: 0,
                    bind: STB_GLOBAL,
                    ty: STT_FUNC,
                    visibility: STV_DEFAULT,
                });
            },
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount => {
                global_syms.push(ElfSym {
                    name: "__fentry__".to_string(),
                    section: SHN_UNDEF,
                    value: 0,
                    size: 0,
                    bind: STB_GLOBAL,
                    ty: STT_FUNC,
                    visibility: STV_DEFAULT,
                });
            },
            crate::cli::FtraceMode::None => {},
        }

        // -mrecord-mcount: generate __mcount_loc section
        if matches!(self.target.ftrace_mode, crate::cli::FtraceMode::RecordMCount)
            && !self.mcount_loc_entries.is_empty()
        {
            let mut mcount_data = Vec::with_capacity(self.mcount_loc_entries.len() * 8);
            for &call_pc in &self.mcount_loc_entries {
                mcount_data.extend_from_slice(&0u64.to_le_bytes()); // placeholder
                let _ = call_pc; // addend set below via relocations
            }
            let sec_idx = self.extra_sections.len();
            self.extra_sections.push(ElfSection {
                name: "__mcount_loc".to_string(),
                data: mcount_data,
                ty: SHT_PROGBITS,
                flags: SHF_ALLOC,
                align: 8,
                group_name: None,
            });
            // Create relocations: each entry is R_X86_64_64 relative to .text section
            // .text section symbol will be at sym_index 1 (after null symbol)
            // We use symbol name ".text" which will be resolved later
            let mut mcount_relas = Vec::new();
            for (i, &call_pc) in self.mcount_loc_entries.iter().enumerate() {
                mcount_relas.push(ElfRela {
                    offset: (i * 8) as u64,
                    sym_index: 0, // .text section sym, patched below
                    rel_type: R_X86_64_64,
                    addend: call_pc as i64,
                });
            }
            // Store for later processing; target_sec_index = 7 + sec_idx
            self.extra_rela_sections.push(ExtraRelaSection {
                name: ".rela__mcount_loc".to_string(),
                relocations: mcount_relas,
                target_sec_index: 7 + sec_idx as u32,
            });
        }

        // 函数定义符号 (包括嵌套函数)
        let asm_func_names: std::collections::BTreeSet<String> = self
            .asm_func_symbols
            .iter()
            .chain(self.global_asm_exports.iter())
            .cloned()
            .collect();
        for name in asm_func_names {
            if let Some(&off) = self.labels.get(&name) {
                // 汇编 .type/@function 或 .globl 导出的函数: 计算 size,
                // 否则 st_size=0 会让 objtool 无法确定函数边界。
                let size = self
                    .func_sizes
                    .get(&name)
                    .copied()
                    .unwrap_or_else(|| self.compute_func_size_fallback(&name, off));
                let (section, value) = self
                    .pushsection_code_ranges
                    .iter()
                    .find(|(start, range_size, _, _)| off >= *start && off < *start + *range_size)
                    .map(|(start, _range_size, sec_idx, sec_off)| {
                        ((*sec_idx + 7) as u16, *sec_off + (off - *start))
                    })
                    .unwrap_or((1u16, off));
                let sym = ElfSym {
                    name: name.clone(),
                    section,
                    value,
                    size,
                    bind: if self.global_asm_exports.contains(&name) {
                        STB_GLOBAL
                    } else {
                        STB_LOCAL
                    },
                    ty: STT_FUNC,
                    visibility: STV_DEFAULT,
                };
                if self.global_asm_exports.contains(&name) {
                    global_syms.push(sym);
                } else {
                    local_syms.push(sym);
                }
            }
        }
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                // __attribute__((copy(fn))) 设置 alias_name - 跳过符号创建, 由 alias 路径处理
                if f.alias_name.is_some() {
                    continue;
                }
                // Skip functions with empty names (can happen from C++ lowering)
                let sym_name = self.resolve_asm_label(&f.name.name).to_string();
                if sym_name.is_empty() {
                    continue;
                }
                if f.body.is_some() {
                    // 收集此函数的嵌套函数 (递归, 不含自身)
                    let mut nested = vec![];
                    if let Some(body) = &f.body {
                        Self::collect_nested_funcs(&body.items, &mut nested);
                    }
                    // 先处理顶层函数自身
                    // Use resolved asm label name for label lookup
                    let label_key = self.resolve_asm_label(&f.name.name).to_string();
                    if let Some(&func_start) = self.labels.get(&label_key) {
                        let func_size =
                            self.func_sizes.get(&label_key).copied().unwrap_or_else(|| {
                                self.compute_func_size_fallback(&label_key, func_start)
                            });
                        let (sec_idx, sym_value) =
                            if let Some(&(si, off)) = custom_func_offsets.get(&label_key) {
                                (si, off)
                            } else {
                                (1u16, func_start)
                            };
                        let sym = ElfSym {
                            name: sym_name.clone(),
                            section: sec_idx,
                            value: sym_value,
                            size: func_size,
                            bind: if f.is_weak || f.is_inline {
                                STB_WEAK
                            } else if f.is_static {
                                STB_LOCAL
                            } else {
                                STB_GLOBAL
                            },
                            // ifunc / target_clones 属性: 函数符号类型为 STT_GNU_IFUNC
                            ty: if f.ifunc_resolver.is_some() || !f.target_clones.is_empty() {
                                STT_GNU_IFUNC
                            } else {
                                STT_FUNC
                            },
                            visibility: Self::parse_visibility(&f.visibility),
                        };
                        if f.is_static {
                            local_syms.push(sym);
                        } else {
                            global_syms.push(sym);
                        }
                    }
                    // 再处理嵌套函数
                    for func in &nested {
                        let orig_nested_name = &func.name.name;
                        // 查找重命名后的名称 (如果有)
                        let effective_name = self
                            .nested_func_renames
                            .get(&f.name.name)
                            .and_then(|m| m.get(orig_nested_name))
                            .cloned()
                            .unwrap_or_else(|| orig_nested_name.clone());
                        let nested_sym_name = self.resolve_asm_label(&effective_name).to_string();
                        if nested_sym_name.is_empty() {
                            continue;
                        }
                        if let Some(&func_start) = self.labels.get(&effective_name) {
                            let func_size = self
                                .func_sizes
                                .get(&effective_name)
                                .copied()
                                .unwrap_or_else(|| {
                                    self.compute_func_size_fallback(&effective_name, func_start)
                                });
                            let (sec_idx, sym_value) = if let Some(&(si, off)) =
                                custom_func_offsets.get(&effective_name)
                            {
                                (si, off)
                            } else {
                                (1u16, func_start)
                            };
                            let sym = ElfSym {
                                name: nested_sym_name,
                                section: sec_idx,
                                value: sym_value,
                                size: func_size,
                                bind: STB_LOCAL, // 嵌套函数使用局部绑定
                                ty: STT_FUNC,
                                visibility: STV_DEFAULT,
                            };
                            local_syms.push(sym);
                        }
                    }
                }
            }
        }
        // 发射所有嵌套函数的符号 (处理深层嵌套或未被上层循环处理的情况)
        for nested_name in self.nested_funcs.keys() {
            // 跳过已发射的符号
            if self.sym_index.contains_key(nested_name) {
                continue;
            }
            if let Some(&func_start) = self.labels.get(nested_name) {
                let func_size = self
                    .func_sizes
                    .get(nested_name)
                    .copied()
                    .unwrap_or_else(|| self.compute_func_size_fallback(nested_name, func_start));
                let (sec_idx, sym_value) =
                    if let Some(&(si, off)) = custom_func_offsets.get(nested_name) {
                        (si, off)
                    } else {
                        (1u16, func_start)
                    };
                local_syms.push(ElfSym {
                    name: nested_name.clone(),
                    section: sec_idx,
                    value: sym_value,
                    size: func_size,
                    bind: STB_LOCAL,
                    ty: STT_FUNC,
                    visibility: STV_DEFAULT,
                });
            }
        }

        // __attribute__((alias)): 创建别名符号
        // alias 函数可能没有函数体 (只是声明), 如:
        //   int alias_func(void) __attribute__((alias("real_func")));
        // f.name.name = "alias_func", f.alias_name = Some("real_func")
        // 需要创建符号 alias_func 指向 real_func 的位置
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if let Some(ref alias_target) = f.alias_name {
                    // 找到目标函数的符号，创建别名指向同一位置
                    if let Some(target_sym) = global_syms
                        .iter()
                        .find(|s| s.name == *alias_target)
                        .or_else(|| local_syms.iter().find(|s| s.name == *alias_target))
                    {
                        let alias_sym = ElfSym {
                            name: f.name.name.clone(),
                            section: target_sym.section,
                            value: target_sym.value,
                            size: target_sym.size,
                            bind: STB_GLOBAL,
                            ty: STT_FUNC,
                            visibility: STV_DEFAULT,
                        };
                        global_syms.push(alias_sym);
                    }
                }
            }
        }

        // __attribute__((target_clones(...))): 创建变体符号别名
        // 对每个 target_clones 函数 foo("sse4.2,default")，
        // 创建 foo.sse4.2 和 foo.default 作为 foo 的别名符号。
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if !f.target_clones.is_empty() {
                    if let Some(func_sym) = global_syms
                        .iter()
                        .find(|s| s.name == f.name.name)
                        .or_else(|| local_syms.iter().find(|s| s.name == f.name.name))
                    {
                        // 克隆字段以避免借用冲突
                        let sec = func_sym.section;
                        let val = func_sym.value;
                        let sz = func_sym.size;
                        for variant in &f.target_clones {
                            let variant_name = format!("{}.{}", f.name.name, variant);
                            let variant_sym = ElfSym {
                                name: variant_name,
                                section: sec,
                                value: val,
                                size: sz,
                                bind: STB_GLOBAL,
                                ty: STT_FUNC,
                                visibility: STV_DEFAULT,
                            };
                            global_syms.push(variant_sym);
                        }
                    }
                }
            }
        }

        // __attribute__((alias)): 变量别名符号
        // int alias_var __attribute__((alias("real_var")));
        // v.name.name = "alias_var", v.alias_name = Some("real_var")
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                if let Some(ref alias_target) = v.alias_name {
                    if let Some(target_sym) = global_syms
                        .iter()
                        .find(|s| s.name == *alias_target)
                        .or_else(|| local_syms.iter().find(|s| s.name == *alias_target))
                    {
                        let alias_sym = ElfSym {
                            name: v.name.name.clone(),
                            section: target_sym.section,
                            value: target_sym.value,
                            size: target_sym.size,
                            bind: STB_GLOBAL,
                            ty: STT_OBJECT,
                            visibility: STV_DEFAULT,
                        };
                        global_syms.push(alias_sym);
                    }
                }
            }
        }

        // &&label 地址引用: 转换为 PC32 重定位 (ET_REL)
        // LEA reg, [RIP+disp32]: disp32 = label - (disp32_pos + 4)
        // R_X86_64_PC32: disp32 = S + A - P, P = disp32_pos
        // 需要 S = label, A = -4, 使 disp32 = label - disp32_pos - 4
        for (disp32_pos, label_name) in self.label_addr_refs.drain(..) {
            self.pending_code_rels.push(PendingCodeRel {
                offset: disp32_pos,
                symbol: label_name,
                rel_type: R_X86_64_PC32,
                addend: -4,
            });
        }

        // 隐式调用的外部函数 (sanitizer 桩 / trapv / 内建库函数) 在源码中
        // 未声明, 必须补 UND 符号, 否则链接器无法解析 call 重定位
        // (符号索引 0 → 目标落到地址 0, 运行时段错误)
        let mut known_syms: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in global_syms.iter().chain(local_syms.iter()) {
            known_syms.insert(s.name.clone());
        }
        for rel in &self.pending_code_rels {
            if let Some(&pos) = self.labels.get(&rel.symbol) {
                if !known_syms.contains(&rel.symbol) {
                    let (section, value) = self
                        .pushsection_code_ranges
                        .iter()
                        .find(|(start, size, _, _)| pos >= *start && pos < *start + *size)
                        .map(|(start, _size, sec_idx, sec_off)| {
                            ((*sec_idx + 7) as u16, *sec_off + (pos - *start))
                        })
                        .unwrap_or((1u16, pos));
                    local_syms.push(ElfSym {
                        name: rel.symbol.clone(),
                        section,
                        value,
                        size: 0,
                        bind: STB_LOCAL,
                        ty: STT_NOTYPE,
                        visibility: STV_DEFAULT,
                    });
                    known_syms.insert(rel.symbol.clone());
                }
            }
        }
        // Also create local symbols for labels referenced from
        // data section (&&label in static initializers)
        for (_, var_name, _, _, _) in &self.pending_data64_rels {
            if let Some(&pos) = self.labels.get(var_name) {
                if !known_syms.contains(var_name) {
                    let (section, value) = self
                        .pushsection_code_ranges
                        .iter()
                        .find(|(start, size, _, _)| pos >= *start && pos < *start + *size)
                        .map(|(start, _size, sec_idx, sec_off)| {
                            ((*sec_idx + 7) as u16, *sec_off + (pos - *start))
                        })
                        .unwrap_or((1u16, pos));
                    local_syms.push(ElfSym {
                        name: var_name.clone(),
                        section,
                        value,
                        size: 0,
                        bind: STB_LOCAL,
                        ty: STT_NOTYPE,
                        visibility: STV_DEFAULT,
                    });
                    known_syms.insert(var_name.clone());
                }
            }
        }
        let mut implicit_und: std::collections::HashSet<String> = std::collections::HashSet::new();
        // 收集所有需要符号表的函数名
        let mut all_func_names: Vec<&String> =
            self.pending_func_rels.iter().map(|(_, n)| n).collect();
        all_func_names.extend(self.pending_func_addr_rels.iter().map(|(_, n, _)| n));
        for name in all_func_names {
            if name == "__dcc_tls_base" {
                continue;
            }
            // 解析 asm label: 用解析后的名字查找/创建符号
            let resolved = self
                .asm_label_map
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.clone());
            if !known_syms.contains(&resolved) && implicit_und.insert(resolved.clone()) {
                global_syms.push(ElfSym {
                    name: resolved,
                    section: SHN_UNDEF,
                    value: 0,
                    size: 0,
                    bind: STB_GLOBAL,
                    ty: STT_FUNC,
                    visibility: STV_DEFAULT,
                });
            }
        }

        for rel in &self.pending_code_rels {
            if self.labels.contains_key(&rel.symbol) {
                continue;
            }
            if !known_syms.contains(&rel.symbol) && implicit_und.insert(rel.symbol.clone()) {
                global_syms.push(ElfSym {
                    name: rel.symbol.clone(),
                    section: SHN_UNDEF,
                    value: 0,
                    size: 0,
                    bind: STB_GLOBAL,
                    ty: STT_NOTYPE,
                    visibility: STV_DEFAULT,
                });
            }
        }

        // .rodata section 符号 (LOCAL)
        if !self.rodata.is_empty() {
            local_syms.push(ElfSym {
                name: ".rodata".to_string(),
                section: 2,
                value: 0,
                size: self.rodata.len() as u64,
                bind: STB_LOCAL,
                ty: STT_SECTION,
                visibility: STV_DEFAULT,
            });
        }

        // .data section 符号 (LOCAL): -fbounds-check 的 OutOfBoundsData 与
        // 已初始化全局变量共用 .data, LEA PC32 重定位以它为符号
        if !self.data_section.is_empty() {
            local_syms.push(ElfSym {
                name: ".data".to_string(),
                section: 3,
                value: 0,
                size: self.data_section.len() as u64,
                bind: STB_LOCAL,
                ty: STT_SECTION,
                visibility: STV_DEFAULT,
            });
        }

        // .text section 符号 (LOCAL): ORC unwind 的 .rela.orc_unwind_ip 需要
        // PC32 重定位以 .text section 符号为基址 (与 objtool 输出一致)
        if !self.code.is_empty() {
            local_syms.push(ElfSym {
                name: ".text".to_string(),
                section: 1,
                value: 0,
                size: self.code.len() as u64,
                bind: STB_LOCAL,
                ty: STT_SECTION,
                visibility: STV_DEFAULT,
            });
        }

        // C++ 零成本异常: `.gcc_except_table` 的节符号 (LOCAL)。FDE 里的 LSDA
        // 字段是 `DW_EH_PE_pcrel|sdata4`, 与 GCC 同构地以该节符号 + 节内偏移作
        // addend; 链接器合并节之后 `S - P` 仍然成立 (两者同在一个输出节内平移)。
        if let Some(sec_idx) = self.eh_except_sec_idx {
            let sec = &self.extra_sections[sec_idx];
            local_syms.push(ElfSym {
                name: sec.name.clone(),
                section: (7 + sec_idx) as u16,
                value: 0,
                size: sec.data.len() as u64,
                bind: STB_LOCAL,
                ty: STT_SECTION,
                visibility: STV_DEFAULT,
            });
        }

        // .pushsection 非文本节: 为数字标签引用创建本地符号
        for rel in &self.pending_custom_sec_rels {
            if !self.sym_index.contains_key(&rel.symbol) {
                if let Some(&pos) = self.labels.get(&rel.symbol) {
                    // 若标签位于可执行 pushsection (.fixup/.altinstr_replacement)
                    // 代码区间内, 符号需指向自定义节而非 .text (代码已搬迁).
                    let (section, value) = self
                        .pushsection_code_ranges
                        .iter()
                        .find(|(start, size, _, _)| pos >= *start && pos < *start + *size)
                        .map(|(start, _size, sec_idx, sec_off)| {
                            ((*sec_idx + 7) as u16, *sec_off + (pos - *start))
                        })
                        .unwrap_or((1u16, pos));
                    local_syms.push(ElfSym {
                        name: rel.symbol.clone(),
                        section,
                        value,
                        size: 0,
                        bind: STB_LOCAL,
                        ty: STT_NOTYPE,
                        visibility: STV_DEFAULT,
                    });
                }
            }
        }

        // 构建最终符号表: STN_UNDEF + locals + globals
        self.symbols.clear();
        self.sym_index.clear();
        // [0] STN_UNDEF
        self.symbols.push(ElfSym {
            name: String::new(),
            section: SHN_UNDEF,
            value: 0,
            size: 0,
            bind: STB_LOCAL,
            ty: STT_NOTYPE,
            visibility: STV_DEFAULT,
        });
        for sym in &local_syms {
            let idx = self.symbols.len() as u32;
            self.symbols.push(sym.clone());
            self.sym_index.insert(sym.name.clone(), idx);
        }
        for sym in &global_syms {
            let idx = self.symbols.len() as u32;
            self.symbols.push(sym.clone());
            self.sym_index.insert(sym.name.clone(), idx);
        }
    }
}
