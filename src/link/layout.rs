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

use super::object::{FlatSection, InputBundle};
use super::resolve::ResolvedSymbol;
use crate::codegen::elf::{
    SHF_ALLOC, SHF_EXECINSTR, SHF_TLS, SHF_WRITE, SHT_NOBITS, SHT_PROGBITS, SHT_RELA, SHT_STRTAB,
    SHT_SYMTAB, STB_LOCAL, STT_NOTYPE, STT_SECTION,
};
use crate::debug::ArchKind;

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
    /// CET/IBT 安全 PLT 节 (endbr64 + jmp *GOT; push $idx; jmp PLT0)
    PltSec,
    /// GOT-resolved PLT 节 (BIND_NOW 模式, 直接 jmp *GOT 无 lazy stub)
    PltGot,
    Dynamic,
    DynStr,
    DynSym,
    RelDyn,
    RelPlt,
    Note,
    /// GNU property 节 (CET feature marking)
    GnuProperty,
    /// 自定义节（保留 .text.foo 之类）
    Named(String),
}

/// 输出节描述。
#[derive(Debug, Clone)]
pub struct OutputSection {
    pub kind: OutputSectionKind,
    pub name: String,
    pub sh_name_off: u32, // 在 .shstrtab 中的偏移
    pub ty: u32,          // SHT_PROGBITS / SHT_NOBITS
    pub flags: u32,       // SHF_* 标志位
    pub align: u64,
    pub data: Vec<u8>,
    pub size: u64, // 对 NOBITS 是虚拟大小
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
    pub input_sections: Vec<u32>, // 对应的输入节（FlatSection 索引）
}

/// 输出段（program header）描述。
#[derive(Debug, Clone)]
pub struct OutputSegment {
    pub ty: u32,    // PT_LOAD / PT_DYNAMIC / PT_TLS / ...
    pub flags: u32, // PF_R / PF_W / PF_X
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
    /// 输出 vaddr (layout 之后): section.vaddr + value
    pub vaddr: u64,
    pub size: u64,
    pub bind: u8,
    pub ty: u8,
    /// 符号类型辅助字段：对于 STT_GNU_IFUNC 符号, kind = 10
    pub kind: u8,
    pub visibility: u8,
    pub section: u16,          // 输出节索引
    pub shndx_if_section: u16, // STT_SECTION 的 shndx
}

/// 完整链接布局结果。
#[derive(Debug, Clone)]
pub struct Layout {
    pub is_pie: bool,
    pub is_shared: bool,
    pub arch: ArchKind,
    pub base_addr: u64,
    /// 首个节的文件偏移起点 (ELF 头 + phdr 之后, 对齐到 16);
    /// 节 vaddr 起点 = base_addr + file_base, 保证 file_off ≡ vaddr (mod page)。
    /// ET_REL (可重定位) 为 0。
    pub file_base: u64,
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
    /// 动态依赖库名 (`DT_NEEDED` 条目, 例如 `libasan.so`)。
    pub needed: Vec<String>,
    /// 自定义动态链接器路径 (-Wl,--dynamic-linker=)
    pub dynamic_linker: Option<String>,
}

/// 输出重定位（应用于输出节，目标是合并后的符号）。
#[derive(Debug, Clone)]
pub struct OutputRela {
    pub section: u32, // 输出节索引
    pub offset: u64,  // 输出节内偏移
    pub sym: u32,     // 输出符号索引
    pub rel_type: u32,
    pub addend: i64,
    /// 内部标识 (for IFUNC / 调试), 不影响输出
    pub _name: String,
}

impl OutputRela {
    pub fn new(section: u32, offset: u64, sym: u32, rel_type: u32, addend: i64) -> Self {
        Self {
            section,
            offset,
            sym,
            rel_type,
            addend,
            _name: String::new(),
        }
    }
}

pub struct LayoutBuilder {
    pub is_pie: bool,
    pub is_shared: bool,
    pub arch: ArchKind,
    pub is_relocatable: bool,
    pub base_addr: u64,
    pub entry_name: String,
    pub gc_sections: bool,
    pub emit_relocs: bool,
    pub emit_dynamic: bool,
    pub keep_debug: bool,
    /// gc_sections 根符号集 (除 entry 外的额外根)
    pub gc_sections_root: Vec<String>,
    /// DT_NEEDED entries propagated from LinkConfig.
    pub needed: Vec<String>,
    /// 自定义动态链接器路径
    pub dynamic_linker: Option<String>,
    /// 链接器脚本路径
    pub linker_script: Option<std::path::PathBuf>,
}

impl LayoutBuilder {
    pub fn new() -> Self {
        Self {
            is_pie: false,
            is_shared: false,
            arch: ArchKind::X86_64,
            is_relocatable: false,
            base_addr: 0x400000,
            entry_name: "_start".into(),
            gc_sections: true,
            emit_relocs: false,
            emit_dynamic: false,
            keep_debug: true,
            gc_sections_root: Vec::new(),
            needed: Vec::new(),
            dynamic_linker: None,
            linker_script: None,
        }
    }

    /// 计算 layout，分配虚拟地址。
    pub fn build(
        &self,
        bundle: &InputBundle,
        resolved: &[ResolvedSymbol],
    ) -> Result<Layout, String> {
        // 如果提供了链接器脚本，使用它来驱动布局
        if let Some(ref script_path) = self.linker_script {
            return self.build_with_linker脚本(script_path, bundle, resolved);
        }

        let mut sections: Vec<OutputSection> = Vec::new();
        let mut section_index_by_kind: HashMap<OutputSectionKind, u32> = HashMap::new();
        let mut custom_section_indices: HashMap<String, u32> = HashMap::new();

        // 合并同名同类节
        for (i, sec) in bundle.sections.iter().enumerate() {
            let kind = classify_section(&sec.name);
            // 跳过 NULL 节、符号表与重定位节（链接后由链接器自己生成）
            if sec.name.is_empty()
                || sec.name == ".symtab"
                || sec.name == ".strtab"
                || sec.name == ".shstrtab"
                || sec.name.starts_with(".rela")
            {
                continue;
            }
            // debug 节按需保留
            if !self.keep_debug
                && (sec.name == ".debug_info"
                    || sec.name == ".debug_abbrev"
                    || sec.name == ".debug_line"
                    || sec.name == ".debug_str"
                    || sec.name == ".debug_aranges"
                    || sec.name == ".debug_ranges"
                    || sec.name.starts_with(".debug_"))
            {
                continue;
            }

            if let Some(k) = kind {
                let entry = section_index_by_kind.entry(k.clone()).or_insert_with(|| {
                    let new_idx = sections.len() as u32;
                    let k_for_creation = k.clone();
                    let name = output_name(k_for_creation.clone());
                    let flags = section_flags_for_kind(k_for_creation.clone());
                    let align = section_align_for_kind(k_for_creation.clone());
                    let load_segment = if matches!(
                        k_for_creation,
                        OutputSectionKind::Text
                            | OutputSectionKind::Rodata
                            | OutputSectionKind::Init
                            | OutputSectionKind::Fini
                            | OutputSectionKind::Plt
                            | OutputSectionKind::PltSec
                            | OutputSectionKind::PltGot
                    ) {
                        0
                    } else {
                        1
                    };
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
                let entry = custom_section_indices
                    .entry(key.clone())
                    .or_insert_with(|| {
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
        // .interp 节 (动态链接器路径): 必须在布局分配前创建, 参与 vaddr 分配,
        // 并位于首个 PT_LOAD 段内 (内核要求 PT_INTERP 指向可加载段)。
        if let Some(ref interp) = self.dynamic_linker {
            let mut data = interp.as_bytes().to_vec();
            data.push(0); // NUL 终止
            let size = data.len() as u64;
            let new_idx = sections.len() as u32;
            sections.push(OutputSection {
                kind: OutputSectionKind::Named(".interp".into()),
                name: ".interp".into(),
                sh_name_off: 0,
                ty: SHT_PROGBITS,
                flags: SHF_ALLOC,
                align: 1,
                data,
                size,
                entsize: 0,
                output_index: new_idx,
                load_segment: 0,
                vaddr: 0,
                file_off: 0,
                input_offsets: Vec::new(),
                input_sections: Vec::new(),
            });
            section_index_by_kind.insert(OutputSectionKind::Named(".interp".into()), new_idx);
        }

        // 填充数据并计算 input_offsets
        self.fill_section_data(&mut sections, bundle);

        // 分配虚拟地址
        // 首个节文件偏移起点: ELF 头 + 最大 phdr 数之后 (phdr 数量与
        // write_elf_internal 计算一致, 实际写入的 phdr 可能更少, 预留区零填充)。
        // ET_REL 无 LOAD 段/无同余要求, file_base = 0。
        let file_base = if self.is_relocatable {
            0
        } else {
            // PT_PHDR 也计入: PIE/Shared = 7 (PHDR+LOAD+LOAD+STACK+DYNAMIC+RELRO+INTERP),
            // ET_EXEC = 5 (PHDR+LOAD+LOAD+STACK+RELRO)。
            let nphdr_max = if self.emit_dynamic { 7 } else { 5 };
            (64 + nphdr_max * 56 + 15) & !15
        };
        self.assign_addresses(&mut sections, file_base);

        // 重新分配自定义节
        self.assign_custom_sections(&mut sections);

        // 构建输出符号表
        let symbols = self.build_symbols(bundle, resolved, &sections);

        // 段映射
        let segments = self.build_segments(&sections);

        // 入口 (使用 OutputSymbol.vaddr, 它已是 section.vaddr + value)
        let entry = symbols
            .iter()
            .find(|s| s.name == self.entry_name)
            .map(|s| s.vaddr)
            .unwrap_or_else(|| {
                if let Some(s) = symbols.iter().find(|s| s.name == "main") {
                    return s.vaddr;
                }
                sections.first().map(|s| s.vaddr).unwrap_or(self.base_addr)
            });

        // 转换重定位到输出节/输出符号空间
        let output_relas = self.translate_relocations(bundle, resolved, &sections, &symbols);

        // 计算 n_locals
        let n_locals = symbols.iter().filter(|s| s.bind == STB_LOCAL).count() as u32;

        Ok(Layout {
            is_pie: self.is_pie,
            is_shared: self.is_shared,
            arch: self.arch,
            base_addr: self.base_addr,
            file_base,
            sections,
            segments,
            symbols,
            entry,
            has_symtab: true,
            has_dynamic: self.emit_dynamic,
            n_locals,
            output_relas,
            needed: self.needed.clone(),
            dynamic_linker: self.dynamic_linker.clone(),
        })
    }

    /// 使用链接器脚本驱动布局
    fn build_with_linker脚本(
        &self,
        script_path: &std::path::Path,
        bundle: &InputBundle,
        resolved: &[ResolvedSymbol],
    ) -> Result<Layout, String> {
        // 1. 读取并解析链接器脚本
        let script_text = std::fs::read_to_string(script_path)
            .map_err(|e| format!("failed to read linker script: {}", e))?;
        let script = super::linker_script::LinkerScript::parse(&script_text)?;

        // 2. 执行脚本，获取布局结果
        let script_result = super::linker_script::execute_script(&script, bundle, resolved)?;

        // 3. 检查断言失败
        if !script_result.assert_failures.is_empty() {
            return Err(format!(
                "linker script assertions failed: {}",
                script_result.assert_failures.join(", ")
            ));
        }

        // 4. 将 ScriptLayoutResult 转换为 Layout
        self.convert_script_result_to_layout(script_result, bundle, resolved)
    }

    /// 将脚本布局结果转换为内部 Layout 结构
    fn convert_script_result_to_layout(
        &self,
        script_result: super::linker_script::ScriptLayoutResult,
        bundle: &InputBundle,
        resolved: &[ResolvedSymbol],
    ) -> Result<Layout, String> {
        let mut sections: Vec<OutputSection> = Vec::new();

        // 转换脚本定义的输出节
        for script_sec in &script_result.sections {
            if script_sec.is_discard || script_sec.is_info {
                continue;
            }

            let out_sec = OutputSection {
                kind: OutputSectionKind::Named(script_sec.name.clone()),
                name: script_sec.name.clone(),
                sh_name_off: 0,
                ty: script_sec.ty,
                flags: script_sec.flags,
                align: script_sec.align,
                data: script_sec.data.clone(),
                size: script_sec.size,
                entsize: 0,
                output_index: sections.len() as u32,
                load_segment: if script_sec.flags & SHF_WRITE != 0 { 1 } else { 0 },
                vaddr: script_sec.vma,
                file_off: 0, // computed below
                input_offsets: script_sec
                    .input_sections
                    .iter()
                    .map(|m| m.offset)
                    .collect(),
                input_sections: script_sec
                    .input_sections
                    .iter()
                    .map(|m| m.input_index)
                    .collect(),
            };
            sections.push(out_sec);
        }

        // 脚本已经填充了数据，不需要再调用 fill_section_data
        // 脚本已经计算了地址，不需要再调用 assign_addresses
        // 只需计算文件偏移
        let file_base = if self.is_relocatable {
            0
        } else {
            let nphdr_max = if self.emit_dynamic { 7 } else { 5 };
            (64 + nphdr_max * 56 + 15) & !15
        };
        let mut current_file_off = file_base;
        for sec in sections.iter_mut() {
            let align = sec.align.max(1);
            current_file_off = (current_file_off + align - 1) & !(align - 1);
            sec.file_off = current_file_off;
            if sec.ty != SHT_NOBITS {
                current_file_off += sec.data.len() as u64;
            }
        }

        // 构建输出符号表
        let mut symbols = self.build_symbols(bundle, resolved, &sections);

        // 添加脚本定义的符号
        for script_sym in &script_result.symbols {
            symbols.push(OutputSymbol {
                name: script_sym.name.clone(),
                value: script_sym.value,
                vaddr: script_sym.value,
                size: 0,
                bind: 0, // STB_LOCAL
                ty: 0,   // STT_NOTYPE
                kind: 0,
                visibility: 0,
                section: 0,
                shndx_if_section: 0,
            });
        }

        // 段映射
        let segments = self.build_segments(&sections);

        // 入口地址
        let entry = script_result
            .entry
            .unwrap_or_else(|| {
                symbols
                    .iter()
                    .find(|s| s.name == self.entry_name)
                    .map(|s| s.vaddr)
                    .unwrap_or_else(|| {
                        sections.first().map(|s| s.vaddr).unwrap_or(self.base_addr)
                    })
            });

        // 转换重定位
        let output_relas = self.translate_relocations(bundle, resolved, &sections, &symbols);

        // 计算 n_locals
        let n_locals = symbols.iter().filter(|s| s.bind == STB_LOCAL).count() as u32;

        Ok(Layout {
            is_pie: self.is_pie,
            is_shared: self.is_shared,
            arch: self.arch,
            base_addr: self.base_addr,
            file_base,
            sections,
            segments,
            symbols,
            entry,
            has_symtab: true,
            has_dynamic: self.emit_dynamic,
            n_locals,
            output_relas,
            needed: self.needed.clone(),
            dynamic_linker: self.dynamic_linker.clone(),
        })
    }

    fn assign_custom_sections(&self, _sections: &mut [OutputSection]) {
        // 暂未做自定义节独立布局，标准合并已覆盖
    }

    fn fill_section_data(&self, sections: &mut [OutputSection], bundle: &InputBundle) {
        for sec in sections.iter_mut() {
            if sec.input_sections.is_empty() {
                continue;
            }
            if sec.ty == SHT_NOBITS {
                continue;
            } // NOBITS 不需要数据
            if matches!(
                sec.kind,
                OutputSectionKind::Got
                    | OutputSectionKind::GotPlt
                    | OutputSectionKind::Plt
                    | OutputSectionKind::PltSec
                    | OutputSectionKind::PltGot
                    | OutputSectionKind::GnuProperty
                    | OutputSectionKind::RelDyn
                    | OutputSectionKind::RelPlt
                    | OutputSectionKind::Dynamic
                    | OutputSectionKind::DynSym
                    | OutputSectionKind::DynStr
            ) {
                // 动态节在 plt_got.rs 中填充
                continue;
            }
            let mut cur: u64 = 0;
            for &in_idx in &sec.input_sections {
                let in_idx = in_idx as usize;
                if in_idx >= bundle.sections.len() {
                    continue;
                }
                let in_sec = &bundle.sections[in_idx];
                let a = in_sec.align.max(1);
                let aligned = (cur + a - 1) & !(a - 1);
                // 填充对齐字节，使 data 与 input_offsets 一致
                let pad = aligned - cur;
                if pad > 0 {
                    sec.data.extend(std::iter::repeat_n(0, pad as usize));
                }
                sec.input_offsets.push(aligned);
                sec.data.extend_from_slice(&in_sec.data);
                cur = aligned + in_sec.data.len() as u64;
            }
            sec.size = cur;
        }
    }

    fn run_gc(&self, sections: &mut [OutputSection], bundle: &InputBundle) {
        // 实现 gc-sections:
        // 1. 收集根符号 (entry + gc_sections_root)
        // 2. 从根出发做引用图 BFS, 标记 live 节
        // 3. 移除未标记的节
        if !self.gc_sections {
            return;
        }

        use std::collections::{HashSet, VecDeque};

        // 1. 根符号集合
        let mut roots: HashSet<String> = HashSet::new();
        roots.insert(self.entry_name.clone());
        // 如果用户没有指定 gc_sections_root，自动选择所有全局符号作为根
        if self.gc_sections_root.is_empty() {
            for sym in &bundle.symbols {
                if !sym.name.is_empty() && sym.section != 0 {
                    // STB_GLOBAL = 1, STB_WEAK = 2
                    if sym.bind == 1 || sym.bind == 2 {
                        roots.insert(sym.name.clone());
                    }
                }
            }
        } else {
            for r in &self.gc_sections_root {
                roots.insert(r.clone());
            }
        }

        // 2. 找 .init_array / .fini_array / .init / .fini / .text.* 等始终保留的节
        let mut live: HashSet<u32> = HashSet::new();
        for (i, sec) in sections.iter().enumerate() {
            if matches!(
                sec.name.as_str(),
                ".init"
                    | ".fini"
                    | ".init_array"
                    | ".fini_array"
                    | ".text.startup"
                    | ".text.exit"
                    | ".ctors"
                    | ".dtors"
                    | ".note"
                    | ".note.GNU-stack"
                    | ".note.gnu.build-id"
                    | ".dynamic"
                    | ".dynsym"
                    | ".dynstr"
                    | ".got"
                    | ".got.plt"
                    | ".plt"
                    | ".rela.dyn"
                    | ".rela.plt"
                    | ".gnu.hash"
                    | ".gnu.version"
                    | ".gnu.version_d"
                    | ".gnu.version_r"
                    | ".interp"
                    | ".eh_frame"
                    | ".eh_frame_hdr"
                    | ".tdata"
                    | ".tbss"
                    | ".data.rel.ro"
                    | ".fini"
                    | ".init"
                    | ".bss"
            ) {
                live.insert(i as u32);
            }
        }
        // SHF_GNU_RETAIN 标志的节也保留
        for (i, sec) in sections.iter().enumerate() {
            if sec.flags & 0x200000 != 0 {
                // SHF_GNU_RETAIN
                live.insert(i as u32);
            }
        }

        // 3. 引用图 BFS
        let mut queue: VecDeque<String> = roots.into_iter().collect();
        let mut seen_syms: HashSet<String> = HashSet::new();
        // 构建: symbol_name -> section_index
        let sym_to_sec: HashMap<String, u32> = HashMap::new();
        for (i, sec) in sections.iter().enumerate() {
            if sec.flags & 0x200000 != 0 {
                // 标记所有符号
                for r in &bundle.relocations {
                    if r.section as usize == i {
                        // 这个重定位所在节必活
                        live.insert(i as u32);
                    }
                }
            }
        }
        let _ = sym_to_sec;
        // 收集所有重定位引用的符号
        while let Some(sym) = queue.pop_front() {
            if !seen_syms.insert(sym.clone()) {
                continue;
            }
            // 找这个符号所在的节
            for (i, sec) in sections.iter().enumerate() {
                if sec.name == sym || sec.kind == OutputSectionKind::Named(sym.clone()) {
                    live.insert(i as u32);
                }
            }
        }
        // 简化 BFS: 对所有 .text.* 节, 看其是否被 entry / gc_sections_root 引用
        // 启发式: 把所有带 "main" / "_start" / 用户指定 gc_sections_root 的节保留
        for root_name in &self.gc_sections_root {
            for (i, sec) in sections.iter().enumerate() {
                if sec.name == format!(".text.{}", root_name)
                    || sec.name.ends_with(&format!("/{}", root_name))
                    || sec.name.ends_with(&format!(".{}", root_name))
                {
                    live.insert(i as u32);
                }
            }
        }
        // 4. 对未 live 的节, 把 size/data 清零 (软删除)
        for (i, sec) in sections.iter_mut().enumerate() {
            if !live.contains(&(i as u32))
                && sec.size > 0
                && sec.ty != crate::codegen::elf::SHT_NOBITS
            {
                // 软删除: 把 data 清空, size=0, 标记为不分配
                // 注意: 实际 "完全删除" 需重新分配 vaddr; 简化处理只清 data
                // 这样 layout 阶段生成的 ELF 中该节 size 仍可见, 但内容为空
                // 真实实现需要修改 assign_addresses
            }
        }
        // 5. 真正"删除": 对于未 live 的非 NONE 节, 标记 output_index = 0 (软删除)
        for (i, sec) in sections.iter_mut().enumerate() {
            if !live.contains(&(i as u32)) && sec.size > 0 {
                // 软删除, 标记为不输出 (output_index = u32::MAX)
                sec.output_index = u32::MAX;
                sec.size = 0;
                sec.data.clear();
            }
        }
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

        // CET/IBT 安全 PLT 节 (.plt.sec)
        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::PltSec,
            name: ".plt.sec".to_string(),
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
        idx_map.insert(OutputSectionKind::PltSec, new_idx);

        // GOT-resolved PLT 节 (.plt.got, BIND_NOW 模式)
        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::PltGot,
            name: ".plt.got".to_string(),
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
        idx_map.insert(OutputSectionKind::PltGot, new_idx);

        // GNU property 节 (CET feature marking)
        let new_idx = sections.len() as u32;
        sections.push(OutputSection {
            kind: OutputSectionKind::GnuProperty,
            name: ".gnu.property".to_string(),
            sh_name_off: 0,
            ty: SHT_PROGBITS,
            flags: SHF_ALLOC,
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
        idx_map.insert(OutputSectionKind::GnuProperty, new_idx);

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

    fn assign_addresses(&self, sections: &mut [OutputSection], file_base: u64) {
        assign_section_addrs(sections, self.base_addr, file_base)
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
            vaddr: 0,
            size: 0,
            bind: STB_LOCAL,
            ty: STT_NOTYPE,
            kind: 0,
            visibility: 0,
            section: 0,
            shndx_if_section: 0,
        });

        // 为每个输出节生成 STT_SECTION 符号
        for sec in sections {
            if sec.flags & SHF_ALLOC == 0 {
                continue;
            }
            out.push(OutputSymbol {
                name: sec.name.clone(),
                value: 0,
                vaddr: sec.vaddr,
                size: 0,
                bind: STB_LOCAL,
                ty: STT_SECTION,
                kind: 0,
                visibility: 0,
                section: sec.output_index as u16,
                shndx_if_section: sec.output_index as u16,
            });
        }

        // 跨 TU 解析后的 GLOBAL/WEAK 符号
        // 注意: r.section 是 bundle 内的 flat section 索引, 需要映射到输出节
        // r.value 是输入节内的偏移, 需要加上输入节在输出节内的偏移
        for r in resolved {
            if r.bind == STB_LOCAL {
                continue;
            }
            // 未定义符号 (section==0): 加入 .symtab 以便 nm 显示为 U
            // 使用 u16::MAX 作为未定义符号的 section 哨兵值
            if r.section == 0 {
                out.push(OutputSymbol {
                    name: r.name.clone(),
                    value: 0,
                    vaddr: 0,
                    size: r.size,
                    bind: r.bind,
                    ty: r.ty,
                    kind: r.ty,
                    visibility: r.visibility,
                    section: u16::MAX, // SHN_UNDEF 哨兵
                    shndx_if_section: 0,
                });
                continue;
            }
            let in_offset = find_input_offset(sections, r.section, 0).unwrap_or(0);
            let out_sec_idx = match find_output_section_for_input_idx(sections, r.section) {
                Some(v) => v,
                None => continue,
            };
            let sec = &sections[out_sec_idx];
            let val = r.value + in_offset;
            out.push(OutputSymbol {
                name: r.name.clone(),
                value: val,
                vaddr: sec.vaddr.wrapping_add(val),
                size: r.size,
                bind: r.bind,
                ty: r.ty,
                kind: r.ty,
                visibility: r.visibility,
                section: sec.output_index as u16,
                shndx_if_section: 0,
            });
        }

        // LOCAL 符号
        for (i, sym) in bundle.symbols.iter().enumerate() {
            if sym.bind != STB_LOCAL {
                continue;
            }
            if sym.section == 0 {
                continue;
            }
            if sym.name.is_empty() {
                continue;
            }
            let in_offset = find_input_offset(sections, sym.section, 0).unwrap_or(0);
            let out_sec_idx = match find_output_section_for_input_idx(sections, sym.section) {
                Some(v) => v,
                None => continue,
            };
            let sec = &sections[out_sec_idx];
            let val = sym.value + in_offset;
            out.push(OutputSymbol {
                name: sym.name.clone(),
                value: val,
                vaddr: sec.vaddr.wrapping_add(val),
                size: sym.size,
                bind: sym.bind,
                ty: sym.ty,
                kind: sym.ty,
                visibility: sym.visibility,
                section: sec.output_index as u16,
                shndx_if_section: 0,
            });
            let _ = i;
        }
        out
    }

    fn build_segments(&self, sections: &[OutputSection]) -> Vec<OutputSegment> {
        let mut seg0: Option<OutputSegment> = None;
        let mut seg1: Option<OutputSegment> = None;
        for sec in sections {
            let flags = if sec.flags & SHF_EXECINSTR != 0 {
                5
            }
            // PF_R | PF_X
            else if sec.flags & SHF_WRITE != 0 {
                6
            }
            // PF_R | PF_W
            else {
                4
            }; // PF_R
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
            if in_sec_idx >= bundle.sections.len() {
                continue;
            }
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
            if sym_global >= bundle.symbols.len() {
                continue;
            }
            let bundle_sym = &bundle.symbols[sym_global];
            // 找对应输出符号
            let out_sym_idx = name_to_output_sym.get(&bundle_sym.name).copied();
            let out_sym = match out_sym_idx {
                Some(v) => v,
                None => continue, // undefined 符号且未在 allowed 中 → 已报错
            };
            let _ = resolved;
            out.push(OutputRela {
                section: out_sec as u32,
                offset: input_off + r.offset,
                sym: out_sym as u32,
                rel_type: r.rel_type,
                addend: r.addend,
                _name: String::new(),
            });
        }
        out
    }
}

/// 按段顺序为输出节分配 vaddr (段 0: .text/.plt/.rodata/.init/.fini; 段 1: .data 等)。
/// 幂等, 可重复调用: 链接后期向 .text 追加入口桩后需重算,
/// 否则文件偏移 (write_elf_internal 实时计算) 与 vaddr 错位。
pub fn assign_section_addrs(sections: &mut [OutputSection], base_addr: u64, file_base: u64) {
    // 节 vaddr 起点 = base_addr + file_base: 文件偏移起点 (file_base) 与
    // vaddr 起点 (base_addr) 都同余 0 (mod page), 保证 file_off ≡ vaddr (mod page),
    // 首个 PT_LOAD 段才能从文件偏移 0 开始 (覆盖 ELF 头 + phdr, AT_PHDR 需要)。
    let mut v0 = base_addr + file_base;
    let mut f0 = file_base;
    // 先给段 0 分配, 得到实际 vaddr/文件跨度, 再定段 1 基址,
    // 保证段间不重叠且文件偏移与 vaddr 同余 (offset % page == vaddr % page)。
    for sec in sections.iter_mut() {
        if sec.load_segment != 0 {
            continue;
        }
        let a = sec.align.max(1);
        v0 = align_up_to(v0, a);
        f0 = align_up_to(f0, a);
        sec.vaddr = v0;
        sec.file_off = f0;
        v0 += sec.size;
        f0 += sec.size;
    }
    // 段 1 基址: 段 0 末尾 page 对齐后 + 0x1000 (load segment 间隔一个 page)
    let mut v1 = align_up_to(v0, 0x1000) + 0x1000;
    let mut f1 = align_up_to(f0, 0x1000) + 0x1000;

    // 其余段
    for sec in sections.iter_mut() {
        if sec.load_segment == 0 {
            continue;
        }
        let a = sec.align.max(1);
        v1 = align_up_to(v1, a);
        f1 = align_up_to(f1, a);
        sec.vaddr = v1;
        sec.file_off = f1;
        v1 += sec.size;
        f1 += sec.size;
    }
}

fn classify_section(name: &str) -> Option<OutputSectionKind> {
    match name {
        ".text" => Some(OutputSectionKind::Text),
        ".rodata" | ".rodata.str1.1" | ".rodata.str4.4" | ".rodata.str8.8" => {
            Some(OutputSectionKind::Rodata)
        }
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
        OutputSectionKind::Text
        | OutputSectionKind::Plt
        | OutputSectionKind::PltSec
        | OutputSectionKind::PltGot => SHF_ALLOC | SHF_EXECINSTR,
        OutputSectionKind::Rodata | OutputSectionKind::Init | OutputSectionKind::Fini => SHF_ALLOC,
        OutputSectionKind::Data
        | OutputSectionKind::Bss
        | OutputSectionKind::InitArray
        | OutputSectionKind::FiniArray
        | OutputSectionKind::Got
        | OutputSectionKind::GotPlt
        | OutputSectionKind::Dynamic
        | OutputSectionKind::DynSym
        | OutputSectionKind::DynStr => SHF_ALLOC | SHF_WRITE,
        OutputSectionKind::Tdata => SHF_ALLOC | SHF_WRITE | SHF_TLS,
        OutputSectionKind::Tbss => SHF_ALLOC | SHF_WRITE | SHF_TLS,
        OutputSectionKind::RelDyn | OutputSectionKind::RelPlt => SHF_ALLOC,
        OutputSectionKind::Note | OutputSectionKind::GnuProperty => SHF_ALLOC,
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
        OutputSectionKind::Got
        | OutputSectionKind::GotPlt
        | OutputSectionKind::Dynamic
        | OutputSectionKind::DynSym
        | OutputSectionKind::DynStr
        | OutputSectionKind::RelDyn
        | OutputSectionKind::RelPlt => 8,
        OutputSectionKind::Plt | OutputSectionKind::PltSec | OutputSectionKind::PltGot => 16,
        OutputSectionKind::Note => 4,
        OutputSectionKind::GnuProperty => 8,
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
        OutputSectionKind::PltSec => ".plt.sec",
        OutputSectionKind::PltGot => ".plt.got",
        OutputSectionKind::Dynamic => ".dynamic",
        OutputSectionKind::DynStr => ".dynstr",
        OutputSectionKind::DynSym => ".dynsym",
        OutputSectionKind::RelDyn => ".rela.dyn",
        OutputSectionKind::RelPlt => ".rela.plt",
        OutputSectionKind::Note => ".note",
        OutputSectionKind::GnuProperty => ".gnu.property",
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

fn find_output_section_for_input_idx(
    sections: &[OutputSection],
    bundle_sec_idx: u32,
) -> Option<usize> {
    for (idx, sec) in sections.iter().enumerate() {
        if sec.input_sections.contains(&bundle_sec_idx) {
            return Some(idx);
        }
    }
    None
}

fn find_input_offset(sections: &[OutputSection], in_idx: u32, _orig_section: u32) -> Option<u64> {
    // 遍历所有输出节, 查找包含输入节 in_idx 的那个
    for sec in sections {
        for (j, &input_sec) in sec.input_sections.iter().enumerate() {
            if input_sec == in_idx {
                return sec.input_offsets.get(j).copied();
            }
        }
    }
    None
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
