// src/codegen/x86_64/elf/symbols.rs
// ET_EXEC symbol table collection.

use crate::ast::*;
use crate::codegen::x86_64::Codegen;
use crate::codegen::shared::elf::{
    ElfSym, SHN_UNDEF, STB_GLOBAL, STB_LOCAL, STB_WEAK, STT_FUNC, STT_GNU_IFUNC, STT_NOTYPE,
    STT_OBJECT, STT_SECTION, STT_TLS, STV_DEFAULT,
};

impl Codegen {
    pub fn collect_exec_symbols(&mut self, prog: &Program) {
        self.symbols.clear();
        self.sym_index.clear();
        self.symbols.push(ElfSym {
            name: String::new(),
            section: SHN_UNDEF,
            value: 0,
            size: 0,
            bind: STB_LOCAL,
            ty: STT_NOTYPE,
            visibility: STV_DEFAULT,
        });
        if !self.rodata.is_empty() {
            self.symbols.push(ElfSym {
                name: ".rodata".into(),
                section: 2,
                value: 0,
                size: self.rodata.len() as u64,
                bind: STB_LOCAL,
                ty: STT_SECTION,
                visibility: STV_DEFAULT,
            });
        }
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                if v.is_extern && v.init.is_none() {
                    continue;
                }
                let Some(g) = self.globals_raw.get(&v.name.name) else {
                    continue;
                };
                let ty = if g.is_tls { STT_TLS } else { STT_OBJECT };
                self.symbols.push(ElfSym {
                    name: self.resolve_asm_label(&v.name.name).to_string(),
                    section: g.section,
                    value: g.offset,
                    size: g.size as u64,
                    bind: if v.is_static { STB_LOCAL } else { STB_GLOBAL },
                    ty,
                    visibility: Self::parse_visibility(&v.visibility),
                });
            }
        }
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                let resolved_name = self.resolve_asm_label(&f.name.name).to_string();
                if f.body.is_none() {
                    // 外部函数声明: 注册为 undefined 符号 (SHN_UNDEF).
                    // 使 resolve() 能识别已知外部符号并发出诊断而非 panic.
                    if !self.sym_index.contains_key(&resolved_name) {
                        self.symbols.push(ElfSym {
                            name: resolved_name,
                            section: SHN_UNDEF,
                            value: 0,
                            size: 0,
                            bind: STB_GLOBAL,
                            ty: STT_FUNC,
                            visibility: STV_DEFAULT,
                        });
                    }
                    continue;
                }
                if let Some(&off) = self.labels.get(&resolved_name) {
                    let sz = self.func_sizes.get(&f.name.name).copied().unwrap_or(0);
                    // ifunc 属性: 函数符号类型为 STT_GNU_IFUNC
                    let sym_type = if f.ifunc_resolver.is_some() {
                        STT_GNU_IFUNC
                    } else {
                        STT_FUNC
                    };
                    self.symbols.push(ElfSym {
                        name: resolved_name,
                        section: 1,
                        value: off,
                        size: sz,
                        bind: if f.is_static {
                            STB_LOCAL
                        } else if f.is_weak || f.is_inline {
                            // GNU89 inline 语义: 非 static inline 函数发射为 weak 符号,
                            // 避免多个编译单元包含同一头文件时产生多重定义
                            STB_WEAK
                        } else {
                            STB_GLOBAL
                        },
                        ty: sym_type,
                        visibility: Self::parse_visibility(&f.visibility),
                    });
                }
            }
        }
        // 发射嵌套函数的符号 (它们不在 prog.decls 中)
        for nested_name in self.nested_funcs.keys() {
            if let Some(&off) = self.labels.get(nested_name) {
                let sz = self.func_sizes.get(nested_name).copied().unwrap_or(0);
                // 避免重复发射 (如果已经在上面的循环中处理过)
                if !self.sym_index.contains_key(nested_name) {
                    self.symbols.push(ElfSym {
                        name: nested_name.clone(),
                        section: 1,
                        value: off,
                        size: sz,
                        bind: STB_LOCAL, // 嵌套函数使用局部绑定
                        ty: STT_FUNC,
                        visibility: STV_DEFAULT,
                    });
                }
            }
        }
        // Static 局部变量符号: 在 self.globals_raw 中但不在 prog.decls 中
        // 收集已处理的变量名, 避免重复
        let mut emitted_var_syms: std::collections::HashSet<String> = std::collections::HashSet::new();
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                emitted_var_syms.insert(self.resolve_asm_label(&v.name.name).to_string());
            }
        }
        for (name, g) in &self.globals_raw {
            if emitted_var_syms.contains(name.as_str()) {
                continue;
            }
            if self.defined_funcs.contains(name.as_str()) {
                continue;
            }
            if name.is_empty() || g.section == SHN_UNDEF {
                continue;
            }
            // Static local 的 section: 3=.data, 4=.bss
            // 在 ET_EXEC 中, data_section 紧跟在 rodata+tdata 之后
            self.symbols.push(ElfSym {
                name: name.clone(),
                section: g.section,
                value: g.offset,
                size: g.size as u64,
                bind: STB_LOCAL,
                ty: STT_OBJECT,
                visibility: STV_DEFAULT,
            });
            emitted_var_syms.insert(name.clone());
        }
        // ELF 规范要求: 所有 LOCAL 符号必须出现在 GLOBAL/WEAK 符号之前
        // 按 bind 排序: STB_LOCAL(0) < STB_GLOBAL(1) < STB_WEAK(2)
        // 保留第一个 null 符号不动 (index 0)
        let null_sym = self.symbols[0].clone();
        self.symbols.remove(0);
        self.symbols.sort_by_key(|s| s.bind);
        self.symbols.insert(0, null_sym);
        for (i, sym) in self.symbols.iter().enumerate() {
            if !sym.name.is_empty() {
                self.sym_index.insert(sym.name.clone(), i as u32);
            }
        }
    }
}
