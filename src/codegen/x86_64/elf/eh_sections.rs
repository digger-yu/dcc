//! src/codegen/x86_64/elf/eh_sections.rs
//!
//! C++ 零成本异常 (zero-cost EH) 的 ELF 侧装配: `.gcc_except_table` (LSDA)、
//! `.eh_frame` (FDE 的 `zPLR` 增强与 LSDA 字段)、以及两者需要的重定位。
//!
//! 为什么必须在**两个不同时点**做:
//!
//! - LSDA 字节与 `.gcc_except_table` 节必须在符号表定稿 (`collect_rel_symbols`)
//!   **之前**建好: FDE 的 LSDA 字段要引用 `.gcc_except_table` 的节符号, 而 ELF
//!   规定 `.symtab` 里所有局部符号必须排在全局符号之前, 定稿后无法再插入。
//! - 类型表的重定位 (PC32 指向 `DW.ref._ZTIxxx`) 必须在符号表定稿**之后**做,
//!   因为它按符号名查 `sym_index`。

use crate::codegen::shared::elf::{ElfRela, ElfSection, R_X86_64_PC32, SHF_ALLOC, SHT_PROGBITS};
use crate::codegen::x86_64::eh_tables::build_lsda;
use crate::codegen::x86_64::Codegen;

/// `.gcc_except_table` 的段对齐: GCC 用 4 (类型表项是 4 字节)。
const EXCEPT_TABLE_ALIGN: u64 = 4;

impl Codegen {
    /// 把每个函数的 LSDA 编进 `.gcc_except_table` 并记下偏移。
    ///
    /// 必须在 `collect_rel_symbols` 之前调用 (见模块级注释)。无 EH 函数时什么都不做,
    /// 因此不产生任何节, 也不影响纯 C 模块的产物字节。
    pub fn assemble_eh_lsda(&mut self) {
        if self.eh_funcs.is_empty() {
            return;
        }
        let mut data: Vec<u8> = Vec::new();
        // 发射序 = 函数在 `.text` 里的**地址序**, 而不是 `eh_funcs` 的登记序:
        // 并行代码生成的 chunk 完成顺序不定, 按登记序会让同一输入产生不同
        // 的 `.gcc_except_table` 字节 (产物不可复现)。调用点同理按 start 排序。
        let mut ordered: Vec<&crate::codegen::x86_64::eh_tables::EhFuncInfo> =
            self.eh_funcs.iter().collect();
        ordered.sort_by_key(|f| f.text_start);
        for func in ordered {
            let Some(&func_start) = self.labels.get(&func.name) else {
                // 函数没有发射 (被 DCE 掉的声明) → 不会有 FDE, 跳过。
                continue;
            };
            // FDE 只描述 `labels[name]` 指向的那一份代码。同名被发射多次时 (前端把
            // 重载合并成同一个链接名的病态输入), 其余副本对链接器不可达, 给它编
            // LSDA 只会得到一张「call-site 区间不属于本 FDE」的错表。
            if func.text_start != func_start {
                continue;
            }
            let mut sites = func.sites.clone();
            sites.sort_by_key(|s| s.start);
            let image = build_lsda(func_start, &sites, &self.labels);
            // 落地垫标签解析不出来 = 该受保护区间会被静默丢掉, 异常因此直接穿过
            // 本帧 (既不析构也不捕获)。这是后端不变式被破坏, 必须报错而不是发错表。
            for label in &image.unresolved_sites {
                self.diagnostics.push(crate::diagnostics::Diagnostic {
                    sev: crate::diagnostics::Severity::Error,
                    code: "E1124",
                    msg: format!(
                        "landing pad label '{}' of function '{}' was registered as a                          protected call site but never emitted",
                        label, func.name
                    ),
                    span: crate::diagnostics::Span::point("<eh>", 0, 0),
                    hint: Some(
                        "受保护调用的落地垫必须由 lowering 发射; 缺失会让异常跳过本帧的                          析构与 handler, 属于后端缺陷而非用户代码问题".into(),
                    ),
                    source_line: None,
                });
            }
            let lsda_off = data.len() as u64;
            data.extend_from_slice(&image.bytes);
            for (rel_off, sym) in image.type_relocs {
                self.eh_lsda_relocs.push((lsda_off + rel_off, sym));
            }
            self.eh_lsda_offsets.insert(func.name.clone(), lsda_off);
        }
        if data.is_empty() {
            return;
        }
        let idx = self.extra_sections.len();
        self.extra_sections.push(ElfSection {
            name: ".gcc_except_table".to_string(),
            data,
            ty: SHT_PROGBITS,
            flags: SHF_ALLOC,
            align: EXCEPT_TABLE_ALIGN,
            group_name: None,
        });
        self.eh_except_sec_idx = Some(idx);
    }

    /// 符号表定稿后发射 `.rela.gcc_except_table`。
    ///
    /// 类型表项是 `DW_EH_PE_pcrel | DW_EH_PE_indirect | DW_EH_PE_sdata4`, 即
    /// `.long DW.ref._ZTIxxx - .`。PC32 已经隐含减去本字段地址, 所以 addend = 0。
    /// 符号缺失时**报错而不是发错表**: 表与符号不一致会让 unwinder 在运行时读
    /// 到野指针, 比编译失败严重得多。
    pub fn emit_eh_relocations(&mut self) {
        let Some(sec_idx) = self.eh_except_sec_idx else {
            return;
        };
        if self.eh_lsda_relocs.is_empty() {
            return;
        }
        let mut relocations = Vec::with_capacity(self.eh_lsda_relocs.len());
        for (offset, sym) in std::mem::take(&mut self.eh_lsda_relocs) {
            let Some(&sym_index) = self.sym_index.get(&sym) else {
                self.diagnostics.push(crate::diagnostics::Diagnostic {
                    sev: crate::diagnostics::Severity::Error,
                    code: "E1122",
                    msg: format!(
                        "exception handling needs symbol '{}' (the indirect slot for a \
                         catch type), but it is missing from this translation unit",
                        sym
                    ),
                    span: crate::diagnostics::Span::point("<eh>", 0, 0),
                    hint: Some("catch 类型必须有 typeinfo 对象; 确认该类型已定义且未被裁剪".into()),
                    source_line: None,
                });
                continue;
            };
            relocations.push(ElfRela {
                offset,
                sym_index,
                rel_type: R_X86_64_PC32,
                addend: 0,
            });
        }
        if relocations.is_empty() {
            return;
        }
        self.extra_rela_sections
            .push(crate::codegen::shared::elf::ExtraRelaSection {
                name: ".rela.gcc_except_table".to_string(),
                relocations,
                target_sec_index: 7 + sec_idx as u32,
            });
    }
}
