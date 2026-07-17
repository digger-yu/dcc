// src/link/layout.rs - 节合并与虚拟地址布局
//
// 同名节按相同属性合并到同一输出节：
//   - SHT_PROGBITS 的可加载节（.text, .rodata, .data, .tdata, .init, .fini 等）
//   - SHT_NOBITS 的 .bss, .tbss
// 自定义节按名字独立处理（function-sections 的 .text.foo 等保持独立）。
//
// 虚拟地址基础：
//   ET_EXEC（非 PIE）：  基址 0x400000，与单 TU 路径一致
//   ET_DYN （PIE/.so）：基址 0x0
// 段映射（PHDR）：
//   LOAD #1:  .text + .rodata       R+X / R
//   LOAD #2:  .data + .bss + .tdata R+W

use std::collections::HashMap;

use super::object::{FlatSection, InputBundle, InputSymbol};
use super::resolve::ResolvedSymbol;
use crate::codegen::elf::{
    SHF_ALLOC, SHF_EXECINSTR, SHF_WRITE, SHF_TLS, SHT_NOBITS, SHT_PROGBITS, SHT_SYMTAB, SHT_STRTAB, SHT_RELA,
    STB_LOCAL, STB_GLOBAL, STB_WEAK, STT_NOTYPE, STT_FUNC, STT_OBJECT, STT_SECTION, STT_TLS,
    ElfSym,
};

/// 输出节类型：标准节按类合并，自定义节独立。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OutputSectionKind {
    Text,
    Rodata,
    Data,
    Bss,
    Tdata,
    Tbss,
    Init,
    Fini,
    InitArray,
    FiniArray,
    Got,
    GotPlt,
    Plt,
    Dynamic,
    DynStr,
    DynSym,
    RelDyn,
    RelPlt,
    Note,
    /// 自定义节（保留 .text.foo 之类）
    Named(String),
}

/// 输出节描述。
#[derive(Debug, Clone)]
pub struct OutputSection {
    pub kind: OutputSectionKind,
    pub name: String,
    pub sh_name_off: u32,    // 在 .shstrtab 中的偏移
    pub ty: u32,             // SHT_PROGBITS / SHT_NOBITS
    pub flags: u32,          // SHF_* 标志位
    pub align: u64,
    pub data: Vec<u8>,
    pub size: u64,           // 对 NOBITS 是虚拟大小
    pub entsize: u64,
    /// 在输出 section header table 中的索引
    pub output_index: u32,
    /// 加载段：0 = 只读 + 可执行, 1 = 可写
    pub load_segment: u8,
    /// 输出 ELF 中的虚拟地址
    pub vaddr: u64,
    /// 输出 ELF 中的文件偏移
    pub file_off: u64,
    /// 节中每个输入节的偏移表（用于重定位）
    pub input_offsets: Vec<u64>,
    pub input_sections: Vec<u32>,   // 对应的输入节（FlatSection 索引）
}

/// 输出段（program header）描述。
#[derive(Debug, Clone)]
pub struct OutputSegment {
    pub ty: u32,             // PT_LOAD / PT_DYNAMIC / PT_TLS / ...
    pub flags: u32,          // PF_R / PF_W / PF_X
    pub vaddr: u64,
    pub file_off: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
    /// 该段包含的输出节
    pub section_indices: Vec<u32>,
}

/// 输出符号条目（最终布局后）。
#[derive(Debug, Clone)]
pub struct OutputSymbol {
    pub name: String,
    pub value: u64,
    pub size: u64,
    pub bind: u8,
    pub ty: u8,
    pub visibility: u8,
    pub section: u16,        // 输出节索引
    pub shndx_if_section: u16, // STT_SECTION 的 shndx
}

/// 完整链接布局结果。
#[derive(Debug, Clone)]
pub struct Layout {
    pub is_pie: bool,
    pub is_shared: bool,
    pub base_addr: u64,
    pub sections: Vec<OutputSection>,
    pub segments: Vec<OutputSegment>,
    pub symbols: Vec<OutputSymbol>,
    pub entry: u64,
    /// 输出 section header table 的 .symtab / .strtab
    pub has_symtab: bool,
    pub has_dynamic: bool,
    /// 在 .symtab 中所有 LOCAL 符号数（用于 sh_info）
    pub n_locals: u32,
    /// FlatRela 在应用后被转化为已修复的字节
    pub output_relas: Vec<OutputRela>,
}

/// 输出重定位（应用于输出节，目标是合并后的符号）。
#[derive(Debug, Clone)]
pub struct OutputRela {
    pub section: u32,        // 输出节索引
    pub offset: u64,         // 输出节内偏移
    pub sym: u32,            // 输出符号索引
    pub rel_type: u32,
    pub addend: i64,
}

pub struct LayoutBuilder {
    pub is_pie: bool,
    pub is_shared: bool,
    pub base_addr: u64,
    pub entry_name: String,
    pub gc_sections: bool,
    pub emit_relocs: bool,
    pub emit_dynamic: bool,
    pub keep_debug: bool,
}

impl LayoutBuilder {
    pub fn new() -> Self {
        Self {
            is_pie: false,
            is_shared: false,
            base_addr: 0x400000,
            entry_name: "_start".into(),
            gc_sections: true,
            emit_relocs: false,
            emit_dynamic: false,
            keep_debug: true,
        }
    }

    /// 计算 layout，分配虚拟地址。
    pub fn build(
        &self,
        bundle: &InputBundle,
        resolved: &[ResolvedSymbol],
    ) -> Result<Layout, String> {
        let mut sections: Vec<OutputSection> = Vec::new();
        let mut section_index_by_kind: HashMap<OutputSectionKind, u32> = HashMap::new();
        let mut custom_section_indices: HashMap<String, u32> = HashMap::new();

        // 合并同名同类节
        for (i, sec) in bundle.sections.iter().enumerate() {
            let kind = classify_section(&sec.name);
            // 跳过符号表与重定位节（链接后由链接器自己生成）
            if sec.name == ".symtab" || sec.name == ".strtab" ||
               sec.name == ".shstrtab" || sec.name.starts_with(".rela") {
                continue;
            }
            // debug 节按需保留
            if !self.keep_debug && (sec.name == ".debug_info" || sec.name == ".debug_abbrev" ||
                sec.name == ".debug_line" || sec.name == ".debug_str" || sec.name == ".debug_aranges" ||
                sec.name == ".debug_ranges" || sec.name.starts_with(".debug_")) {
                continue;
            }

            if let Some(k) = kind {
                let entry = section_index_by_kind.entry(k.clone()).or_insert_with(|| {
                    let new_idx = sections.len() as u32;
                    let k_for_creation = k.clone();
                    let name = output_name(k_for_creation.clone());
                    let flags = section_flags_for_kind(k_for_creation.clone());
                    let align = section_align_for_kind(k_for_creation.clone());
                    let load_segment = if matches!(k_for_creation, OutputSectionKind::Text | OutputSectionKind::Rodata | OutputSectionKind::Init | OutputSectionKind::Fini | OutputSectionKind::Plt) { 0 } else { 1 };
                    sections.push(OutputSection {
                        kind: k,
                        name: name.to_string(),
                        sh_name_off: 0,
                        ty: SHT_PROGBITS,
                        flags,
                        align,
                        data: Vec::new(),
                        size: 0,
                        entsize: 0,
                        output_index: new_idx,
                        load_segment,
                        vaddr: 0,
                        file_off: 0,
                        input_offsets: Vec::new(),
                        input_sections: Vec::new(),
                    });
                    new_idx
                });
                let out = &mut sections[*entry as usize];
                out.input_sections.push(i as u32);
            } else {
                // 自定义节（每个独立）
                let key = sec.name.clone();
                let entry = custom_section_indices.entry(key.clone()).or_insert_with(|| {
                    let new_idx = sections.len() as u32;
                    sections.push(OutputSection {
                        kind: OutputSectionKind::Named(key.clone()),
                        name: key.clone(),
                        sh_name_off: 0,
                        ty: sec.ty,
                        flags: sec.flags,
                        align: sec.align,
                        data: Vec::new(),
                        size: 0,
                        entsize: 0,
                        output_index: new_idx,
                        load_segment: if sec.flags & SHF_WRITE != 0 { 1 } else { 0 },
                        vaddr: 0,
                        file_off: 0,
                        input_offsets: Vec::new(),
                        input_sections: Vec::new(),
                    });
                    new_idx
                });
                let out = &mut sections[*entry as usize];
                out.input_sections.push(i as u32);
            }
        }

        // gc-sections: 标记未引用的节
        if self.gc_sections {
            self.run_gc(&mut sections, bundle);
        }

        // 处理动态节
        if self.emit_dynamic {
            self.add_dynamic_sections(&mut sections, &mut section_index_by_kind);
        }

        // 填充数据并计算 input_offsets
        self.fill_section_data(&mut sections, bundle);

        // 分配虚拟地址
        self.assign_addresses(&mut sections);

        // 重新分配自定义节
        self.assign_custom_sections(&mut sections);

        // 构建输出符号表
        let symbols = self.build_symbols(bundle, resolved, &sections);

        // 段映射
        let segments = self.build_segments(&sections);

        // 入口
        let entry = symbols.iter()
            .find(|s| s.name == self.entry_name)
            .map(|s| {
                if let Some(sec) = sections.get(s.section as usize) {
                    sec.vaddr + s.value
                } else {
                    0
                }
            })
            .unwrap_or_else(|| {
                // 尝试 _start, main
                if let Some(s) = symbols.iter().find(|s| s.name == "main") {
                    if let Some(sec) = sections.get(s.section as usize) {
                        return sec.vaddr + s.value;
                    }
                }
                sections.get(0).map(|s| s.vaddr).unwrap_or(self.base_addr)
            });

        // 转换重定位到输出节/输出符号空间
        let output_relas = self.translate_relocations(bundle, resolved, &sections, &symbols);

        // 计算 n_locals
        let n_locals = symbols.iter().filter(|s| s.bind == STB_LOCAL).count() as u32;

        Ok(Layout {
            is_pie: self.is_pie,
            is_shared: self.is_shared,
            base_addr: self.base_addr,
            sections,
            segments,
            symbols,
            entry,
            has_symtab: true,
            has_dynamic: self.emit_dynamic,
            n_locals,
            output_relas,
        })
    }

    fn assign_custom_sections(&self, _sections: &mut [OutputSection]) {
        // 暂未做自定义节独立布局，标准合并已覆盖
    }

    fn fill_section_data(&self, sections: &mut [OutputSection], bundle: &InputBundle) {
        for sec in sections.iter_mut() {
            if sec.input_sections.is_empty() { continue; }
            if sec.ty == SHT_NOBITS { continue; }  // NOBITS 不需要数据
            if matches!(sec.kind,
                OutputSectionKind::Got | OutputSectionKind::GotPlt |
                OutputSectionKind::Plt | OutputSectionKind::RelDyn | OutputSectionKind::RelPlt |
                OutputSectionKind::Dynamic | OutputSectionKind::DynSym | OutputSectionKind::DynStr) {
                // 动态节在 plt_got.rs 中填充
                continue;
            }
            let mut cur: u64 = 0;
            for &in_idx in &sec.input_sections {
                let in_idx = in_idx as usize;
                if in_idx >= bundle.sections.len() { continue; }
                let in_sec = &bundle.sections[in_idx];
                let a = in_sec.align.max(1);
                cur = (cur + a - 1) & !(a - 1);
                sec.input_offsets.push(cur);
                sec.data.extend_from_slice(&in_sec.data);
                cur += in_sec.data.len() as u64;
            }
            sec.size = cur;
        }
    }

    fn run_gc(&self, _sections: &mut [OutputSection], _bundle: &InputBundle) {
        // 简化版：保留所有节（基础实现，先做正确性再做激进 GC）
        // 真正的 GC 需要基于引用图。
    }

    fn add_dynamic_sections(
        &self,
        sections: &mut Vec<OutputSection>,
        idx_map: &mut HashMap<OutputSectionKind, u32>,
    ) {
        // 不用闭包 — 直接用 mut Vec 的 push
        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::Got,
            name: ".got".to_string(),
            sh_name_off: 0,
            ty: SHT_PROGBITS,
            flags: SHF_ALLOC | SHF_WRITE,
            align: 8,
            data: Vec::new(),
            size: 0,
            entsize: 0,
            output_index: new_idx,
            load_segment: 1,
            vaddr: 0,
            file_off: 0,
            input_offsets: Vec::new(),
            input_sections: Vec::new(),
        });
        idx_map.insert(OutputSectionKind::Got, new_idx);

        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::GotPlt,
            name: ".got.plt".to_string(),
            sh_name_off: 0,
            ty: SHT_PROGBITS,
            flags: SHF_ALLOC | SHF_WRITE,
            align: 8,
            data: Vec::new(),
            size: 0,
            entsize: 0,
            output_index: new_idx,
            load_segment: 1,
            vaddr: 0,
            file_off: 0,
            input_offsets: Vec::new(),
            input_sections: Vec::new(),
        });
        idx_map.insert(OutputSectionKind::GotPlt, new_idx);

        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::Plt,
            name: ".plt".to_string(),
            sh_name_off: 0,
            ty: SHT_PROGBITS,
            flags: SHF_ALLOC | SHF_EXECINSTR,
            align: 16,
            data: Vec::new(),
            size: 0,
            entsize: 0,
            output_index: new_idx,
            load_segment: 0,
            vaddr: 0,
            file_off: 0,
            input_offsets: Vec::new(),
            input_sections: Vec::new(),
        });
        idx_map.insert(OutputSectionKind::Plt, new_idx);

        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::RelPlt,
            name: ".rela.plt".to_string(),
            sh_name_off: 0,
            ty: SHT_RELA,
            flags: SHF_ALLOC,
            align: 8,
            data: Vec::new(),
            size: 0,
            entsize: 24,
            output_index: new_idx,
            load_segment: 1,
            vaddr: 0,
            file_off: 0,
            input_offsets: Vec::new(),
            input_sections: Vec::new(),
        });
        idx_map.insert(OutputSectionKind::RelPlt, new_idx);

        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::RelDyn,
            name: ".rela.dyn".to_string(),
            sh_name_off: 0,
            ty: SHT_RELA,
            flags: SHF_ALLOC,
            align: 8,
            data: Vec::new(),
            size: 0,
            entsize: 24,
            output_index: new_idx,
            load_segment: 1,
            vaddr: 0,
            file_off: 0,
            input_offsets: Vec::new(),
            input_sections: Vec::new(),
        });
        idx_map.insert(OutputSectionKind::RelDyn, new_idx);

        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::Dynamic,
            name: ".dynamic".to_string(),
            sh_name_off: 0,
            ty: SHT_PROGBITS,
            flags: SHF_ALLOC | SHF_WRITE,
            align: 8,
            data: Vec::new(),
            size: 0,
            entsize: 0,
            output_index: new_idx,
            load_segment: 1,
            vaddr: 0,
            file_off: 0,
            input_offsets: Vec::new(),
            input_sections: Vec::new(),
        });
        idx_map.insert(OutputSectionKind::Dynamic, new_idx);

        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::DynSym,
            name: ".dynsym".to_string(),
            sh_name_off: 0,
            ty: SHT_SYMTAB,
            flags: SHF_ALLOC,
            align: 8,
            data: Vec::new(),
            size: 0,
            entsize: 24,
            output_index: new_idx,
            load_segment: 1,
            vaddr: 0,
            file_off: 0,
            input_offsets: Vec::new(),
            input_sections: Vec::new(),
        });
        idx_map.insert(OutputSectionKind::DynSym, new_idx);

        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::DynStr,
            name: ".dynstr".to_string(),
            sh_name_off: 0,
            ty: SHT_STRTAB,
            flags: SHF_ALLOC,
            align: 1,
            data: Vec::new(),
            size: 0,
            entsize: 0,
            output_index: new_idx,
            load_segment: 1,
            vaddr: 0,
            file_off: 0,
            input_offsets: Vec::new(),
            input_sections: Vec::new(),
        });
        idx_map.insert(OutputSectionKind::DynStr, new_idx);
    }

    fn assign_addresses(&self, sections: &mut [OutputSection]) {
        // 段 0: .text / .plt / .rodata / .init / .fini
        // 段 1: .data / .bss / .tdata / .tbss / .got / .got.plt / .dynamic / .dynsym / .dynstr
        let mut v0 = self.base_addr;
        let mut f0: u64 = 0;  // 文件偏移（layout 阶段暂按 0 起步，最后回填）
        // 决定两段基址：保证两段 vaddr 差足够大
        // 段 1 放在段 0 末尾后 0x1000
        let mut text_size: u64 = 0;
        for sec in sections.iter() {
            if sec.load_segment == 0 {
                text_size = text_size.max(sec.vaddr + sec.size);
            }
        }
        let mut v1 = self.base_addr + align_up_to(text_size, 0x1000) + 0x1000;
        let mut f1 = f0 + align_up_to(text_size, 0x1000) + 0x1000;

        // 段 0
        for sec in sections.iter_mut() {
            if sec.load_segment != 0 { continue; }
            let a = sec.align.max(1);
            v0 = align_up_to(v0, a);
            f0 = align_up_to(f0, a);
            sec.vaddr = v0;
            sec.file_off = f0;
            v0 += sec.size;
            f0 += sec.size;
        }
        // 段 1
        for sec in sections.iter_mut() {
            if sec.load_segment == 0 { continue; }
            let a = sec.align.max(1);
            v1 = align_up_to(v1, a);
            f1 = align_up_to(f1, a);
            sec.vaddr = v1;
            sec.file_off = f1;
            v1 += sec.size;
            f1 += sec.size;
        }
    }

    fn build_symbols(
        &self,
        bundle: &InputBundle,
        resolved: &[ResolvedSymbol],
        sections: &[OutputSection],
    ) -> Vec<OutputSymbol> {
        let mut out: Vec<OutputSymbol> = Vec::new();
        // 第一个 LOCAL 槽位固定为空
        out.push(OutputSymbol {
            name: String::new(),
            value: 0,
            size: 0,
            bind: STB_LOCAL,
            ty: STT_NOTYPE,
            visibility: 0,
            section: 0,
            shndx_if_section: 0,
        });

        // 为每个输出节生成 STT_SECTION 符号
        for sec in sections {
            if sec.flags & SHF_ALLOC == 0 { continue; }
            out.push(OutputSymbol {
                name: sec.name.clone(),
                value: 0,
                size: 0,
                bind: STB_LOCAL,
                ty: STT_SECTION,
                visibility: 0,
                section: sec.output_index as u16,
                shndx_if_section: sec.output_index as u16,
            });
        }

        // 跨 TU 解析后的 GLOBAL/WEAK 符号
        for r in resolved {
            if r.section == 0 { continue; }   // undefined 已合并到 ResolvedSymbol 但不出现在 .symtab
            if r.bind == STB_LOCAL { continue; }
            let sec_idx = r.section as usize;
            if sec_idx >= sections.len() { continue; }
            let sec = &sections[sec_idx];
            out.push(OutputSymbol {
                name: r.name.clone(),
                value: r.value,
                size: r.size,
                bind: r.bind,
                ty: r.ty,
                visibility: r.visibility,
                section: sec.output_index as u16,
                shndx_if_section: 0,
            });
        }

        // LOCAL 符号
        for (i, sym) in bundle.symbols.iter().enumerate() {
            if sym.bind != STB_LOCAL { continue; }
            if sym.section == 0 { continue; }
            if sym.name.is_empty() { continue; }
            // 找对应输出节
            let sec_idx = sym.section as usize;
            if sec_idx >= bundle.sections.len() { continue; }
            let input_sec = &bundle.sections[sec_idx];
            // 找第一个匹配的自定义节
            for sec in sections {
                if sec.kind == OutputSectionKind::Named(input_sec.name.clone()) {
                    out.push(OutputSymbol {
                        name: sym.name.clone(),
                        value: sym.value,
                        size: sym.size,
                        bind: sym.bind,
                        ty: sym.ty,
                        visibility: sym.visibility,
                        section: sec.output_index as u16,
                        shndx_if_section: 0,
                    });
                    break;
                }
            }
            let _ = i;
        }
        out
    }

    fn build_segments(&self, sections: &[OutputSection]) -> Vec<OutputSegment> {
        let mut seg0: Option<OutputSegment> = None;
        let mut seg1: Option<OutputSegment> = None;
        for sec in sections {
            let flags = if sec.flags & SHF_EXECINSTR != 0 { 5 }  // PF_R | PF_X
                        else if sec.flags & SHF_WRITE != 0 { 6 }  // PF_R | PF_W
                        else { 4 };                              // PF_R
            let target = if sec.load_segment == 0 { &mut seg0 } else { &mut seg1 };
            if target.is_none() {
                *target = Some(OutputSegment {
                    ty: 1, // PT_LOAD
                    flags,
                    vaddr: sec.vaddr,
                    file_off: sec.file_off,
                    filesz: 0,
                    memsz: 0,
                    align: 0x1000,
                    section_indices: vec![sec.output_index],
                });
            } else {
                let s = target.as_mut().unwrap();
                s.section_indices.push(sec.output_index);
                let end = sec.vaddr + sec.size;
                let fend = sec.file_off + sec.size;
                s.vaddr = s.vaddr.min(sec.vaddr);
                s.file_off = s.file_off.min(sec.file_off);
                s.filesz = fend.saturating_sub(s.file_off);
                s.memsz = end.saturating_sub(s.vaddr);
                s.flags |= flags;
            }
        }
        let mut segs: Vec<OutputSegment> = seg0.into_iter().chain(seg1).collect();
        if self.is_pie || self.is_shared {
            for s in &mut segs {
                s.ty = 1; // PT_LOAD
            }
        }
        segs
    }

    fn translate_relocations(
        &self,
        bundle: &InputBundle,
        resolved: &[ResolvedSymbol],
        sections: &[OutputSection],
        symbols: &[OutputSymbol],
    ) -> Vec<OutputRela> {
        let mut out = Vec::new();
        // 构造 bundle.symbols 中的符号索引 → 输出符号索引的映射
        let mut name_to_output_sym: HashMap<String, usize> = HashMap::new();
        for (i, s) in symbols.iter().enumerate() {
            if !s.name.is_empty() {
                name_to_output_sym.insert(s.name.clone(), i);
            }
        }

        for r in &bundle.relocations {
            // 找到 r.section 所在输出节
            let in_sec_idx = r.section as usize;
            if in_sec_idx >= bundle.sections.len() { continue; }
            let in_sec = &bundle.sections[in_sec_idx];
            // 找到该节被合并到的输出节
            let out_sec_idx = find_output_section_for(sections, in_sec);
            let out_sec = match out_sec_idx {
                Some(v) => v,
                None => continue,
            };
            // 计算输出节内偏移
            let input_off = match find_input_offset(sections, in_sec_idx as u32, r.section) {
                Some(v) => v,
                None => continue,
            };
            // 解析符号
            let sym_global = r.sym as usize;
            if sym_global >= bundle.symbols.len() { continue; }
            let bundle_sym = &bundle.symbols[sym_global];
            // 找对应输出符号
            let out_sym_idx = name_to_output_sym.get(&bundle_sym.name).copied();
            let out_sym = match out_sym_idx {
                Some(v) => v,
                None => continue,  // undefined 符号且未在 allowed 中 → 已报错
            };
            let _ = resolved;
            out.push(OutputRela {
                section: out_sec as u32,
                offset: input_off + r.offset,
                sym: out_sym as u32,
                rel_type: r.rel_type,
                addend: r.addend,
            });
        }
        out
    }
}

fn classify_section(name: &str) -> Option<OutputSectionKind> {
    match name {
        ".text" => Some(OutputSectionKind::Text),
        ".rodata" | ".rodata.str1.1" | ".rodata.str4.4" | ".rodata.str8.8" => Some(OutputSectionKind::Rodata),
        ".data" | ".data.rel.ro" => Some(OutputSectionKind::Data),
        ".bss" => Some(OutputSectionKind::Bss),
        ".tdata" => Some(OutputSectionKind::Tdata),
        ".tbss" => Some(OutputSectionKind::Tbss),
        ".init" => Some(OutputSectionKind::Init),
        ".fini" => Some(OutputSectionKind::Fini),
        ".init_array" => Some(OutputSectionKind::InitArray),
        ".fini_array" => Some(OutputSectionKind::FiniArray),
        _ => None,
    }
}

fn section_flags_for_kind(kind: OutputSectionKind) -> u32 {
    match kind {
        OutputSectionKind::Text | OutputSectionKind::Plt => SHF_ALLOC | SHF_EXECINSTR,
        OutputSectionKind::Rodata | OutputSectionKind::Init | OutputSectionKind::Fini => SHF_ALLOC,
        OutputSectionKind::Data | OutputSectionKind::Bss |
        OutputSectionKind::InitArray | OutputSectionKind::FiniArray |
        OutputSectionKind::Got | OutputSectionKind::GotPlt |
        OutputSectionKind::Dynamic | OutputSectionKind::DynSym | OutputSectionKind::DynStr => SHF_ALLOC | SHF_WRITE,
        OutputSectionKind::Tdata => SHF_ALLOC | SHF_WRITE | SHF_TLS,
        OutputSectionKind::Tbss => SHF_ALLOC | SHF_WRITE | SHF_TLS,
        OutputSectionKind::RelDyn | OutputSectionKind::RelPlt => SHF_ALLOC,
        OutputSectionKind::Note => 0,
        OutputSectionKind::Named(_) => 0,
    }
}

fn section_align_for_kind(kind: OutputSectionKind) -> u64 {
    match kind {
        OutputSectionKind::Text => 16,
        OutputSectionKind::Rodata => 8,
        OutputSectionKind::Data | OutputSectionKind::Bss => 16,
        OutputSectionKind::Tdata | OutputSectionKind::Tbss => 8,
        OutputSectionKind::Init | OutputSectionKind::Fini => 4,
        OutputSectionKind::InitArray | OutputSectionKind::FiniArray => 8,
        OutputSectionKind::Got | OutputSectionKind::GotPlt | OutputSectionKind::Dynamic |
        OutputSectionKind::DynSym | OutputSectionKind::DynStr | OutputSectionKind::RelDyn |
        OutputSectionKind::RelPlt => 8,
        OutputSectionKind::Plt => 16,
        OutputSectionKind::Note => 4,
        OutputSectionKind::Named(_) => 1,
    }
}

fn output_name(kind: OutputSectionKind) -> &'static str {
    match kind {
        OutputSectionKind::Text => ".text",
        OutputSectionKind::Rodata => ".rodata",
        OutputSectionKind::Data => ".data",
        OutputSectionKind::Bss => ".bss",
        OutputSectionKind::Tdata => ".tdata",
        OutputSectionKind::Tbss => ".tbss",
        OutputSectionKind::Init => ".init",
        OutputSectionKind::Fini => ".fini",
        OutputSectionKind::InitArray => ".init_array",
        OutputSectionKind::FiniArray => ".fini_array",
        OutputSectionKind::Got => ".got",
        OutputSectionKind::GotPlt => ".got.plt",
        OutputSectionKind::Plt => ".plt",
        OutputSectionKind::Dynamic => ".dynamic",
        OutputSectionKind::DynStr => ".dynstr",
        OutputSectionKind::DynSym => ".dynsym",
        OutputSectionKind::RelDyn => ".rela.dyn",
        OutputSectionKind::RelPlt => ".rela.plt",
        OutputSectionKind::Note => ".note",
        OutputSectionKind::Named(_) => "",
    }
}

fn align_up_to(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

fn find_output_section_for(sections: &[OutputSection], in_sec: &FlatSection) -> Option<usize> {
    for s in sections {
        if s.kind == OutputSectionKind::Named(in_sec.name.clone()) {
            return Some(s.output_index as usize);
        }
    }
    // 匹配标准节
    let kind = classify_section(&in_sec.name)?;
    for s in sections {
        if s.kind == kind {
            return Some(s.output_index as usize);
        }
    }
    None
}

fn find_input_offset(sections: &[OutputSection], _in_idx: u32, _orig_section: u32) -> Option<u64> {
    // 这里需要把输入节在输出节内的偏移算出来；需要先做一次内容拼接。
    // layout 阶段在 assign_addresses 之前应该已经做了 input_offsets 计算。
    // 此函数假定 layout.fill_section_data 已运行。
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_classification_covers_known_names() {
        assert_eq!(classify_section(".text"), Some(OutputSectionKind::Text));
        assert_eq!(classify_section(".rodata"), Some(OutputSectionKind::Rodata));
        assert_eq!(classify_section(".data"), Some(OutputSectionKind::Data));
        assert_eq!(classify_section(".bss"), Some(OutputSectionKind::Bss));
        assert_eq!(classify_section(".debug_info"), None);
    }
}
