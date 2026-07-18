// src/link/plt_got.rs - PLT / GOT 生成
//
// 为动态链接（PIE 可执行 / .so）生成：
//   - .plt:           PLT 入口跳转表（每个外部函数 16 字节 + PLT[0] 共 32 字节桩）
//   - .got.plt:       GOT 表（[0] = _DYNAMIC, [1] = link_map, [2] = _dl_runtime_resolve,
//                     [3..] = 各 PLT 槽）
//   - .rela.plt:      PLT 重定位表（R_X86_64_JUMP_SLOT）
//   - .rela.dyn:      数据重定位表（R_X86_64_GLOB_DAT / R_X86_64_RELATIVE）
//   - .dynamic:       动态节（DT_NEEDED, DT_STRTAB, DT_SYMTAB, DT_STRSZ, DT_SYMENT, ...）
//   - .dynsym:        动态符号表
//   - .dynstr:        动态字符串表

use super::layout::{Layout, OutputSectionKind};
use crate::codegen::elf::{
    R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT,
    SHT_SYMTAB, SHT_STRTAB,
    STB_GLOBAL, STT_FUNC, STV_DEFAULT,
};

/// 收集 .plt / .got.plt 中需要为哪些外部符号创建槽。
pub struct PltGots {
    /// 所有需要 PLT 槽的外部函数名（按出现顺序）
    pub plt_funcs: Vec<String>,
    /// 所有需要 GOT 数据槽的全局变量
    pub got_data: Vec<String>,
}

impl PltGots {
    pub fn from_layout(layout: &Layout) -> Self {
        // 简化：把 layout.symbols 中 section=0 的符号都视为需要动态链接
        let mut plt_funcs = Vec::new();
        let mut got_data = Vec::new();
        for s in &layout.symbols {
            if s.section != 0 { continue; }
            if s.name.is_empty() { continue; }
            if s.ty == STT_FUNC {
                plt_funcs.push(s.name.clone());
            } else {
                got_data.push(s.name.clone());
            }
        }
        Self { plt_funcs, got_data }
    }
}

/// PLT 单个条目 16 字节。`.plt[0]`（32 字节）是桩，所有条目都跳到 GOT 表。
/// 我们的最小实现：直接用绝对跳转，简化 dynamic linker 行为。
///
/// .plt 条目布局（lazy binding 版）：
///   jmp *GOT_ENTRY(%rip)        ; 6 bytes
///   push $index                 ; 5 bytes
///   jmp PLT0                    ; 5 bytes
///                              = 16 bytes
fn emit_plt_entry(plt_base_vaddr: u64, this_vaddr: u64, got_entry_vaddr: u64, index: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    // jmp *GOT_ENTRY(%rip) -> ff 25 <disp32>
    // disp32 = GOT_ENTRY - (this + 6)
    let disp = (got_entry_vaddr as i64) - (this_vaddr as i64 + 6);
    buf.extend_from_slice(&[0xff, 0x25]);
    buf.extend_from_slice(&(disp as i32).to_le_bytes());
    // push $index (32-bit imm: 68 <imm32>)
    buf.push(0x68);
    buf.extend_from_slice(&(index as i32).to_le_bytes());
    // jmp PLT0 (5 bytes: e9 <disp32>)
    let plt0_vaddr = plt_base_vaddr;
    let plt0_disp = (plt0_vaddr as i64) - (this_vaddr as i64 + 16);
    buf.push(0xe9);
    buf.extend_from_slice(&(plt0_disp as i32).to_le_bytes());
    buf
}

/// PLT[0] 桩：32 字节
///   push *GOT[1](%rip)        ; 6 bytes
///   bnd jmp *GOT[2](%rip)     ; 6 bytes (bnd jmp: f2 ff 25 <disp32>)
///   nop dword ptr [rax+0]     ; 4 bytes (padding)
///   <padding>                 ; 16 bytes
fn emit_plt0(plt0_vaddr: u64, got1_vaddr: u64, got2_vaddr: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(32);
    // push *GOT[1](%rip)
    let disp1 = (got1_vaddr as i64) - (plt0_vaddr as i64 + 6);
    buf.extend_from_slice(&[0xff, 0x35]);
    buf.extend_from_slice(&(disp1 as i32).to_le_bytes());
    // bnd jmp *GOT[2](%rip) = f2 ff 25 <disp32>
    let disp2 = (got2_vaddr as i64) - (plt0_vaddr as i64 + 12);
    buf.extend_from_slice(&[0xf2, 0xff, 0x25]);
    buf.extend_from_slice(&(disp2 as i32).to_le_bytes());
    // padding 20 字节
    buf.extend_from_slice(&[0x66, 0x90]);  // nop
    buf.extend_from_slice(&[0x66, 0x90]);
    buf.extend_from_slice(&[0x0f, 0x1f, 0x80, 0x00, 0x00, 0x00, 0x00]); // 7-byte nop
    buf.extend_from_slice(&[0x0f, 0x1f, 0x80, 0x00, 0x00, 0x00, 0x00]); // 7-byte nop
    buf
}

pub fn fill_plt_got(layout: &mut Layout) {
    let plt_gots = PltGots::from_layout(layout);
    let n_plt = plt_gots.plt_funcs.len();
    let n_got_data = plt_gots.got_data.len();

    // 找各节
    let (plt_idx, got_plt_idx, got_idx, dynsym_idx, dynstr_idx,
        rel_plt_idx, rel_dyn_idx, dynamic_idx) = {
        let mut plt_i = None;
        let mut got_plt_i = None;
        let mut got_i = None;
        let mut dynsym_i = None;
        let mut dynstr_i = None;
        let mut rel_plt_i = None;
        let mut rel_dyn_i = None;
        let mut dynamic_i = None;
        for (i, s) in layout.sections.iter().enumerate() {
            match s.kind {
                OutputSectionKind::Plt => plt_i = Some(i),
                OutputSectionKind::GotPlt => got_plt_i = Some(i),
                OutputSectionKind::Got => got_i = Some(i),
                OutputSectionKind::DynSym => dynsym_i = Some(i),
                OutputSectionKind::DynStr => dynstr_i = Some(i),
                OutputSectionKind::RelPlt => rel_plt_i = Some(i),
                OutputSectionKind::RelDyn => rel_dyn_i = Some(i),
                OutputSectionKind::Dynamic => dynamic_i = Some(i),
                _ => {}
            }
        }
        (
            plt_i.expect("PLT section missing"),
            got_plt_i.expect("GOTPLT section missing"),
            got_i.unwrap_or(0),
            dynsym_i.expect("DYNSYM section missing"),
            dynstr_i.expect("DYNSTR section missing"),
            rel_plt_i.expect("RELPLT section missing"),
            rel_dyn_i.expect("RELDYN section missing"),
            dynamic_i.expect("DYNAMIC section missing"),
        )
    };

    // 复制所需 vaddr（防止借用冲突）
    let got_plt_vaddr = layout.sections[got_plt_idx].vaddr;
    let plt_vaddr = layout.sections[plt_idx].vaddr;
    let _got_vaddr = layout.sections[got_idx].vaddr;
    let rel_plt_vaddr = layout.sections[rel_plt_idx].vaddr;
    let rel_dyn_vaddr = layout.sections[rel_dyn_idx].vaddr;
    let dynsym_vaddr = layout.sections[dynsym_idx].vaddr;
    let dynstr_vaddr = layout.sections[dynstr_idx].vaddr;
    let dynamic_vaddr = layout.sections[dynamic_idx].vaddr;
    let dynamic_off = layout.sections[dynamic_idx].file_off;

    // 准备所有数据
    let got_plt_size = (3 + n_plt) * 8;
    let got_data_size = n_got_data * 8;
    // 写 .rela.plt
    let mut rela_plt_data = Vec::with_capacity(n_plt * 24);
    for i in 0..n_plt {
        let got_entry_off = 24 + (i * 8) as u64;
        let sym_idx = (i + 1) as u32;
        rela_plt_data.extend_from_slice(&got_entry_off.to_le_bytes());
        rela_plt_data.extend_from_slice(&((sym_idx as u64) << 32 | R_X86_64_JUMP_SLOT as u64).to_le_bytes());
        rela_plt_data.extend_from_slice(&0i64.to_le_bytes());
    }
    // 写 .dynsym
    let mut dynsym_data = Vec::with_capacity((1 + n_plt) * 24);
    dynsym_data.extend_from_slice(&[0u8; 24]);
    for _ in 0..n_plt {
        dynsym_data.extend_from_slice(&0u32.to_le_bytes());
        let info = (STB_GLOBAL << 4) | STT_FUNC;
        dynsym_data.push(info);
        dynsym_data.push(STV_DEFAULT);
        dynsym_data.extend_from_slice(&0u16.to_le_bytes());
        dynsym_data.extend_from_slice(&0u64.to_le_bytes());
        dynsym_data.extend_from_slice(&0u64.to_le_bytes());
    }
    // 写 .rela.dyn
    let mut rela_dyn_data = Vec::new();
    if n_got_data > 0 {
        for i in 0..n_got_data {
            let got_off = (i * 8) as u64;
            let sym_idx = (1 + n_plt + i) as u32;
            rela_dyn_data.extend_from_slice(&got_off.to_le_bytes());
            rela_dyn_data.extend_from_slice(&((sym_idx as u64) << 32 | R_X86_64_GLOB_DAT as u64).to_le_bytes());
            rela_dyn_data.extend_from_slice(&0i64.to_le_bytes());
        }
    }
    // 写 .dynstr
    let mut dynstr_data = vec![0u8];
    for name in &plt_gots.plt_funcs {
        for byte in name.bytes() { dynstr_data.push(byte); }
        dynstr_data.push(0);
    }
    for name in &plt_gots.got_data {
        for byte in name.bytes() { dynstr_data.push(byte); }
        dynstr_data.push(0);
    }
    // 生成 .plt
    let mut plt_data = Vec::with_capacity(32 + 16 * n_plt);
    let got1 = got_plt_vaddr + 8;
    let got2 = got_plt_vaddr + 16;
    plt_data.extend_from_slice(&emit_plt0(plt_vaddr, got1, got2));
    for i in 0..n_plt {
        let this = plt_vaddr + plt_data.len() as u64;
        let got_entry = got_plt_vaddr + 24 + (i * 8) as u64;
        plt_data.extend_from_slice(&emit_plt_entry(plt_vaddr, this, got_entry, i as u64));
    }
    // 写 .dynamic
    let mut dynamic_data = Vec::new();
    let dynstr_size = dynstr_data.len() as u64;
    let rela_plt_size = rela_plt_data.len() as u64;
    let rela_dyn_size = rela_dyn_data.len() as u64;
    let dynsym_size = dynsym_data.len() as u64;
    // DT_STRTAB = 5
    dynamic_data.extend_from_slice(&5u64.to_le_bytes());
    dynamic_data.extend_from_slice(&dynstr_vaddr.to_le_bytes());
    // DT_SYMTAB = 6
    dynamic_data.extend_from_slice(&6u64.to_le_bytes());
    dynamic_data.extend_from_slice(&dynsym_vaddr.to_le_bytes());
    // DT_STRSZ = 10
    dynamic_data.extend_from_slice(&10u64.to_le_bytes());
    dynamic_data.extend_from_slice(&dynstr_size.to_le_bytes());
    // DT_SYMENT = 11
    dynamic_data.extend_from_slice(&11u64.to_le_bytes());
    dynamic_data.extend_from_slice(&24u64.to_le_bytes());
    // DT_PLTGOT = 3
    dynamic_data.extend_from_slice(&3u64.to_le_bytes());
    dynamic_data.extend_from_slice(&got_plt_vaddr.to_le_bytes());
    // DT_PLTRELSZ = 2
    dynamic_data.extend_from_slice(&2u64.to_le_bytes());
    dynamic_data.extend_from_slice(&rela_plt_size.to_le_bytes());
    // DT_PLTREL = 20
    dynamic_data.extend_from_slice(&20u64.to_le_bytes());
    dynamic_data.extend_from_slice(&7u64.to_le_bytes());
    // DT_JMPREL = 23
    dynamic_data.extend_from_slice(&23u64.to_le_bytes());
    dynamic_data.extend_from_slice(&rel_plt_vaddr.to_le_bytes());
    // DT_RELA = 7
    dynamic_data.extend_from_slice(&7u64.to_le_bytes());
    dynamic_data.extend_from_slice(&rel_dyn_vaddr.to_le_bytes());
    // DT_RELASZ = 8
    dynamic_data.extend_from_slice(&8u64.to_le_bytes());
    dynamic_data.extend_from_slice(&rela_dyn_size.to_le_bytes());
    // DT_RELAENT = 9
    dynamic_data.extend_from_slice(&9u64.to_le_bytes());
    dynamic_data.extend_from_slice(&24u64.to_le_bytes());
    // DT_NULL = 0
    dynamic_data.extend_from_slice(&0u64.to_le_bytes());
    dynamic_data.extend_from_slice(&0u64.to_le_bytes());

    // 现在一次性写回所有节
    {
        let got_plt = &mut layout.sections[got_plt_idx];
        got_plt.data = vec![0; got_plt_size];
        got_plt.size = got_plt_size as u64;
    }
    if n_got_data > 0 {
        let got = &mut layout.sections[got_idx];
        got.data = vec![0; got_data_size];
        got.size = got_data_size as u64;
    }
    {
        let plt = &mut layout.sections[plt_idx];
        plt.data = plt_data;
        plt.size = plt.data.len() as u64;
    }
    {
        let rel_plt = &mut layout.sections[rel_plt_idx];
        rel_plt.data = rela_plt_data;
        rel_plt.size = rel_plt.data.len() as u64;
    }
    {
        let dynsym = &mut layout.sections[dynsym_idx];
        dynsym.data = dynsym_data;
        dynsym.size = dynsym.data.len() as u64;
    }
    {
        let rel_dyn = &mut layout.sections[rel_dyn_idx];
        rel_dyn.data = rela_dyn_data;
        rel_dyn.size = rel_dyn.data.len() as u64;
    }
    {
        let dynstr = &mut layout.sections[dynstr_idx];
        dynstr.data = dynstr_data;
        dynstr.size = dynstr.data.len() as u64;
    }
    {
        let dynamic = &mut layout.sections[dynamic_idx];
        dynamic.data = dynamic_data;
        dynamic.size = dynamic.data.len() as u64;
    }
    let _ = dynamic_vaddr;
    let _ = dynamic_off;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::layout::OutputSectionKind;
    use crate::codegen::elf::SHT_PROGBITS;

    #[test]
    fn plt_entry_has_correct_displacement() {
        // plt_base = 0x401000, this = 0x401000 (first entry after PLT0)
        // got_entry = 0x402010
        let entry = emit_plt_entry(0x401020, 0x401020, 0x402010, 0);
        // disp = 0x402010 - (0x401020 + 6) = 0xfea
        let disp = i32::from_le_bytes(entry[2..6].try_into().unwrap());
        assert_eq!(disp, 0x402010 - 0x401026);
        assert_eq!(entry.len(), 16);
    }
}
