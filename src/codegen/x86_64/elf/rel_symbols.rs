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
    /// 本 TU 里全部「日后会以符号名发一条重定位」的待处理名字 —— symtab 建表与重定位
    /// 回填共用的**唯一**枚举点。
    ///
    /// 为什么必须只有一份: Phase 2 要给每个被引用名补上 symtab 条目 (本文件), Phase 3
    /// (`elf/resolve.rs::reloc_sym_index`) 只能按名字查索引。两处各自枚举一遍必然漂移 ——
    /// 旧版 UND 登记只盖住了 3 条列表, 而发射重定位的列表有 11 条, 剩下的名字在 symtab
    /// 里根本不存在。迁就它们的旧行为是 `sym_index.get(..).unwrap_or(0)`: 把「无此符号」
    /// 降级成 `STN_UNDEF` (索引 0), 链接期按 ELF 规格取 `S = 0`, 重定位静默落到镜像起点。
    ///
    /// `.L` 前缀的内部标号必须排除: 按 GAS/ELF 约定它们不进 symtab, 给它们建 UND 条目
    /// 等于把「发射器漏回填」的缺陷掩盖成合法未定义符号 (那条路径归 E1102/E1103 管)。
    /// 以 `.` 开头的名字一律不进这个名单: `.rodata` / `.text` 这类 **section 符号**已在
    /// local_syms 里定过义, 再补一条同名 UND 会得到两个同名符号并把重定位指到错的节。
    ///
    /// 输出**保序去重**: symtab 条目序直接进入产物字节, 建立在 HashMap 迭代序上会
    /// 让同一份源码编出不一致的 .o。
    pub(crate) fn referenced_reloc_symbols(&self) -> Vec<String> {
        // 只收集引用, 不对 pending 列表做任何写入 —— 保证不动发射与回填语义。
        let mut raw: Vec<&String> = Vec::new();
        raw.extend(self.pending_func_rels.iter().map(|(_, n)| n));
        raw.extend(self.pending_func_addr_rels.iter().map(|(_, n, _)| n));
        raw.extend(self.pending_code_rels.iter().map(|r| &r.symbol));
        raw.extend(self.pending_data_rels.iter().map(|(_, n)| n));
        raw.extend(self.pending_sib_data_rels.iter().map(|(_, n)| n));
        raw.extend(self.pending_tls_rels.iter().map(|(_, n)| n));
        raw.extend(self.pending_data64_rels.iter().map(|(_, n, _, _, _)| n));
        raw.extend(self.pending_rodata_rels.iter().map(|(_, n, _, _)| n));
        raw.extend(self.pending_debug_info_relocs.iter().map(|(_, n)| n));
        raw.extend(self.pending_custom_sec_rels.iter().map(|r| &r.symbol));
        raw.extend(
            self.pending_label_diff_rels
                .iter()
                .flat_map(|(_, a, b, _)| [a, b]),
        );
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for name in raw {
            // 归一化 asm label: 与 `reloc_sym_index` 用的是同一份 asm_label_map。
            let resolved = self
                .asm_label_map
                .get(name.as_str())
                .cloned()
                .unwrap_or_else(|| name.clone());
            if resolved.is_empty() || Self::is_internal_label(&resolved) {
                continue;
            }
            if resolved.starts_with('.') || resolved.starts_with(".L") {
                continue;
            }
            if seen.insert(resolved.clone()) {
                out.push(resolved);
            }
        }
        out
    }

    /// 把一个 `.text` 发射位置翻译成它最终所在的 ELF 节与节内偏移。
    ///
    /// 可执行 pushsection（`.fixup` / `.altinstr_replacement`）的代码会整段搬到自己的
    /// 节里，所以落在已搬走区间内的标号必须指向那个节，不能继续指 `.text`。
    /// 区间边界取左闭右开：`pos == start + size` 是「区间后的下一个 `.text` 字节」，
    /// 归 `.text`。区间**内部**的标号应当已经进了 `pushsection_label_offsets`（在
    /// `close_exec_pushsection` / `emit_pushsection_data` 登记），调用方会先查那张表，
    /// 不会走到这里。
    pub(crate) fn pushsection_sym_loc(&self, pos: u64) -> (u16, u64) {
        self.pushsection_code_ranges
            .iter()
            .find(|(start, size, _, _)| pos >= *start && pos < *start + *size)
            .map(|(start, _size, sec_idx, sec_off)| ((*sec_idx + 7) as u16, *sec_off + (pos - start)))
            .unwrap_or((1u16, pos))
    }

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
                // 住在寄存器里的对象不进符号表：判据读**家表**（收集阶段已解析完的那一份），
                // 不再独立判 `register_name.is_some()` —— 否则一个没认出的寄存器名会让对象
                // 既无存储又无符号（§11.6.3 记的静默消失缺陷）。同一事实只有一处判据。
                if self.global_homes.contains_key(v.name.name.as_str()) {
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
                // 函数类型的无初值声明（`typedef int ft(int); ft f1;`）是**声明**而不是
                // 对象：gcc 发 NOTYPE / size 0 的未定义引用。按对象发会让链接器把
                // 它当数据符号处理（旧实现还给它分过 .bss，见 parser 同名修复）。
                let is_func_decl = matches!(v.ty, crate::ast::Type::Func(..));
                let sym = ElfSym {
                    name: self.resolve_asm_label(&v.name.name).to_string(),
                    section,
                    value: if is_func_decl { 0 } else { g.offset },
                    size: if is_func_decl { 0 } else { g.size as u64 },
                    bind,
                    ty: if is_func_decl {
                        STT_NOTYPE
                    } else if g.is_tls {
                        STT_TLS
                    } else {
                        STT_OBJECT
                    },
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
                    // IFUNC 基名不能发 UND：它们的有效定义是下面 `ifunc_defs` / 本 TU
                    // 合成循环里那条 `STT_GNU_IFUNC` 已定义符号（`st_value` 指向真
                    // resolver 体）。发 UND 会让 `.o` 里「IFUNC 名字未定义」，链接期只能
                    // 报 undefined reference（旧行为：`attr_ifunc.c` 全链接 E0202）。
                    // 判据与 `ifunc::collect_clone_bases` 一致：无体 + 保留 target_clones
                    // 标记 = 已被 `expand_target_clones` 展开的基名。
                    if !f.target_clones.is_empty() || f.ifunc_resolver.is_some() {
                        continue;
                    }
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
                let (section, value) = self.pushsection_sym_loc(off);
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
                    // `target_clones` 的函数体不应再走到这里：`ifunc::expand_target_clones`
                    // 会把基名换成**无体**声明 + 每个变体一份体。基名带体到这里 = 展开
                    // 没跑过，再往下只能产出「IFUNC 与实现体同址」那份旧坏形状。
                    if !f.target_clones.is_empty() {
                        self.report_internal_bug(
                            "E1126",
                            format!(
                                "function '{}' with a body still carries target_clones {:?} at \
                                 symtab time",
                                f.name.name, f.target_clones
                            ),
                            "driver must call ifunc::expand_target_clones before ir::lower; a \
                                 bodiless base name plus one real body per variant is the only \
                                 shape that can carry a resolver",
                        );
                    }
                    // 嵌套函数的符号不在这里发：本函数只能按**顶层父函数**查
                    // `nested_func_renames`，孙层的原名存在中间那层的表里，查不到
                    // 就退回原名 ⇒ 标签查不到 ⇒ 整个符号被静默跳过。全部嵌套函数
                    // 已由 `compile_module` 预扫描以唯一名登进 `nested_funcs`，下面
                    // 那一段是唯一发射入口。
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
                            // `__attribute__((ifunc(...)))` 挂在**有体**函数上时，只能把
                            // 它自己的体当 IFUNC 值（gcc 拒绝那种写法）。`target_clones`
                            // **不再**在这里参与判定：它的 IFUNC 符号由下面 `ifunc_defs`
                            // 那一段以真 resolver 体产出，基名到这里必然无体。
                            ty: if f.ifunc_resolver.is_some() {
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
                }
            }
        }
        // 嵌套函数符号的**唯一**发射入口：事实只有一份（`nested_funcs`，预扫描
        // 按源码序以唯一名登表），不再按 prog.decls 递归重走一遍 AST —— 两套枚举
        // 必然漂移，今天就是漂成了「两个同名同址项」。
        //
        // 遍历序必须固定：`nested_funcs` 是 HashMap，直接 `keys()` 会让 .symtab 里的
        // 符号次序随进程随机（实测同一份源五连编有一次不一致）。
        let mut nested_names: Vec<&String> = self.nested_funcs.keys().collect();
        nested_names.sort();
        for nested_name in nested_names {
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
                            // 别名必须继承声明自己的绑定：内核大量写
                            // `ssize_t __weak cpu_show_gds(...) __attribute__((alias(...)))`
                            // 当「可被架构覆盖的弱桩」。这里写死 STB_GLOBAL 会让
                            // `ld -r vmlinux.o` 把弱桩与强定义报成 multiple definition。
                            bind: if f.is_weak { STB_WEAK } else { STB_GLOBAL },
                            ty: STT_FUNC,
                            visibility: STV_DEFAULT,
                        };
                        global_syms.push(alias_sym);
                    }
                }
            }
        }

        // ====================================================================
        // IFUNC / CPUID 合成体与 IFUNC 基符号
        // ====================================================================
        //
        // 旧形状（本轮之前）：`target_clones` 的基名**有体**，于是基名与每个变体
        // 都以「基名的地址」发成 `STT_GNU_IFUNC` + 一堆同址别名 —— `.o` 里没有
        // CPUID、没有 resolver，整份判定被搬到内置链接器里现拼字节流，链接因此
        // 只能静默从动态降级为静态（docs/missing_features.md 的 IFUNC 行）。
        //
        // 新形状：`ifunc::expand_target_clones` 把每个变体展开成**独立函数体**
        // （上面的函数定义循环按真实地址给它们发 `STB_LOCAL` + `STT_FUNC`），基名
        // 只留无体声明；`ifunc::emit_ifunc_synthesis` 在 `.text` 尾部合成真 resolver。
        // 所以下面两段只登记「不在 `prog.decls` 里的合成体」与「基名的 IFUNC 符号」，
        // 变体别名循环在这里**整块删除**：变体不再需要别名，它有自己的体。
        //
        // 1) 合成函数：`__dcc_cpu_indicator_init` 与每个 `<f>.resolver`。必须在
        //    `known_syms` 建立之前登记，否则下面「按 labels 补符号」那一轮会把它们
        //    发成 `size = 0` 的 `STT_NOTYPE`（`call`/`jmp` 的目标语义全靠 type+size）。
        for sf in &self.ifunc_synth.clone() {
            let Some(func_start) = self.labels.get(&sf.name).copied() else {
                self.report_internal_bug(
                    "E1126",
                    format!(
                        "synthesized function '{}' is on the ifunc synth list but has no label",
                        sf.name
                    ),
                    "emit_ifunc_synthesis must label every body it emits into .text",
                );
                continue;
            };
            let size = self
                .func_sizes
                .get(&sf.name)
                .copied()
                .unwrap_or_else(|| self.compute_func_size_fallback(&sf.name, func_start));
            let sym = ElfSym {
                name: sf.name.clone(),
                // 合成体总是走 `self.emit`，即 `.text`（section 1）；它不可能属于
                // 自定义节，因为 `move_ranges` 快照在合成之前就已经取定了。
                section: 1u16,
                value: func_start,
                size,
                bind: sf.bind,
                ty: STT_FUNC,
                visibility: STV_DEFAULT,
            };
            if sf.bind == STB_LOCAL {
                local_syms.push(sym);
            } else {
                global_syms.push(sym);
            }
        }

        // 2) IFUNC 基符号：`st_value`/`st_size` 取自它自己的 resolver（与 gcc 实测同形：
        //    `IFUNC f` 与 `FUNC f.resolver` 的 value/size 完全一致，见
        //    docs/wp_ifunc_real_fix.md 的 `readelf -sW` 对照表）。
        for d in &self.ifunc_defs.clone() {
            let Some(func_start) = self.labels.get(&d.resolver).copied() else {
                self.report_internal_bug(
                    "E1126",
                    format!(
                        "IFUNC '{}' references resolver '{}' which has no label",
                        d.name, d.resolver
                    ),
                    "the IFUNC symbol's value must be the resolver body; emitting the same \
                     address as the implementation body is the silent-static-downgrade defect \
                     this path exists to remove",
                );
                continue;
            };
            let size = self
                .func_sizes
                .get(&d.resolver)
                .copied()
                .unwrap_or_else(|| self.compute_func_size_fallback(&d.resolver, func_start));
            let sym = ElfSym {
                name: d.name.clone(),
                section: 1u16,
                value: func_start,
                size,
                bind: d.bind,
                ty: STT_GNU_IFUNC,
                visibility: Self::parse_visibility(&d.visibility),
            };
            if d.bind == STB_LOCAL {
                local_syms.push(sym);
            } else {
                global_syms.push(sym);
            }
        }

        // 3) `__attribute__((ifunc("r")))`：基名是 IFUNC，值/大小取自 `r` 的函数体
        //    （gcc 实测：`IFUNC GLOBAL ifunc_func` 与 `FUNC LOCAL resolver` 同 value/size）。
        for decl in &prog.decls {
            let Decl::Func(f) = decl else { continue };
            let Some(resolver) = f.ifunc_resolver.clone() else {
                continue;
            };
            if !f.target_clones.is_empty() {
                continue; // 两个属性同时挂：target_clones 已在上面产出 IFUNC
            }
            if f.body.is_some() {
                continue; // 有体形状由上面的函数定义循环处理（避免同名发两份）
            }
            let name = self.resolve_asm_label(&f.name.name).to_string();
            let resolver_label = self.resolve_asm_label(&resolver).to_string();
            let Some(func_start) = self.labels.get(&resolver_label).copied() else {
                self.report_internal_bug(
                    "E1125",
                    format!(
                        "__attribute__((ifunc(\"{resolver}\"))) on '{name}': resolver is not \
                         defined in this TU (no label '{resolver_label}')",
                    ),
                    "an IFUNC resolver must have a body in the same translation unit; do not \
                     emit an IFUNC symbol whose value cannot point at a resolver",
                );
                continue;
            };
            let size = self
                .func_sizes
                .get(&resolver_label)
                .copied()
                .unwrap_or_else(|| self.compute_func_size_fallback(&resolver_label, func_start));
            let bind = if f.is_weak {
                STB_WEAK
            } else if f.is_static {
                STB_LOCAL
            } else {
                STB_GLOBAL
            };
            let sym = ElfSym {
                name,
                section: 1u16,
                value: func_start,
                size,
                bind,
                ty: STT_GNU_IFUNC,
                visibility: Self::parse_visibility(&f.visibility),
            };
            if bind == STB_LOCAL {
                local_syms.push(sym);
            } else {
                global_syms.push(sym);
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
                            // 同上：变量别名也得分到声明的弱绑定（`__weak`
                            // `struct foo bar __attribute__((alias("baz")))`）。
                            bind: if v.is_weak { STB_WEAK } else { STB_GLOBAL },
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
                    let (section, value) = self.pushsection_sym_loc(pos);
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
                    let (section, value) = self.pushsection_sym_loc(pos);
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

        // 其余「日后会以符号名发一条重定位」的条目: 同一口径补 UND 登记。
        // 旧版只枚举了函数调用表 + pending_code_rels, `pending_data_rels` /
        // `pending_sib_data_rels` / `pending_tls_rels` / `pending_data64_rels` /
        // `pending_rodata_rels` / `pending_debug_info_relocs` / `pending_custom_sec_rels` /
        // `pending_label_diff_rels` 里的名字全靠碰巧命中已注册符号 —— inline asm 引用的
        // 外部符号 (`retf $stack_cleanup` / `ljmp $0x08, far_target`) 因此查无索引。
        for name in self.referenced_reloc_symbols() {
            if !known_syms.contains(&name) && implicit_und.insert(name.clone()) {
                global_syms.push(ElfSym {
                    name,
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
                // 优先从 pushsection_label_offsets 查 (sec_idx, sec_off) —— 这张表在标号
                // 被登记那一刻就记下「它最终所在节的偏移」：非执行 pushsection 的内容从来不
                // 进 self.code，可执行 pushsection 的区间首尾相接（内层 `775:` 与外层 `774:`
                // 是同一个发射位置），拿 .text 位置反查 `pushsection_code_ranges` 必然二义。
                if let Some(&(sec_idx, sec_off)) = self.pushsection_label_offsets.get(&rel.symbol) {
                    local_syms.push(ElfSym {
                        name: rel.symbol.clone(),
                        section: (sec_idx + 7) as u16,
                        value: sec_off,
                        size: 0,
                        bind: STB_LOCAL,
                        ty: STT_NOTYPE,
                        visibility: STV_DEFAULT,
                    });
                    continue;
                }
                if let Some(&pos) = self.labels.get(&rel.symbol) {
                    // 若标签位于可执行 pushsection (.fixup/.altinstr_replacement)
                    // 代码区间内, 符号需指向自定义节而非 .text (代码已搬迁).
                    let (section, value) = self.pushsection_sym_loc(pos);
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
