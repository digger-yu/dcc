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

use super::layout::{Layout, OutputRela, OutputSectionKind};
use super::link_opts::LinkOpts;
use crate::codegen::elf::{
    R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT, R_X86_64_RELATIVE, STB_GLOBAL, STT_FUNC, STT_SECTION,
    STV_DEFAULT,
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
        // 只收集真正未定义 (UND) 的符号。注意: OutputSymbol.section 存的是
        // 输出节索引, 首个输出节 (.text) 的索引是 0 —— 已定义符号 (main/_start
        // 等) 的 section 也可能是 0, 不能仅凭 section==0 判定 UND。
        // 判定条件: section==0 && vaddr==0 (UND 无地址) && 非 STT_SECTION 节符号。
        let mut plt_funcs = Vec::new();
        let mut got_data = Vec::new();
        for s in &layout.symbols {
            if s.section != 0 {
                continue;
            }
            if s.name.is_empty() {
                continue;
            }
            if s.ty == STT_SECTION {
                continue; // .text 等节符号 (output_index 可能为 0)
            }
            if s.vaddr != 0 {
                continue; // 已定义符号有地址, 不需要 PLT/GOT 槽
            }
            if s.ty == STT_FUNC {
                plt_funcs.push(s.name.clone());
            } else {
                got_data.push(s.name.clone());
            }
        }
        Self {
            plt_funcs,
            got_data,
        }
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
///
/// CET/IBT 模式下 .plt 仅包含 PLT[0] (带 endbr64),
/// 每个函数条目放入 .plt.sec 节。
fn emit_plt_entry(
    plt_base_vaddr: u64,
    this_vaddr: u64,
    got_entry_vaddr: u64,
    index: u64,
) -> Vec<u8> {
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

/// CET/IBT 安全 PLT 条目 (放入 .plt.sec 节) 32 字节:
///   endbr64                     ; 4 bytes (f3 0f 1e fa)
///   jmp *GOT_ENTRY(%rip)        ; 6 bytes
///   push $index                 ; 5 bytes
///   jmp PLT0                    ; 5 bytes
///   nop * 6                     ; 6 bytes padding
///                              = 32 bytes (16-byte aligned)
fn emit_plt_sec_entry(
    plt0_vaddr: u64,
    this_vaddr: u64,
    got_entry_vaddr: u64,
    index: u64,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(32);
    // endbr64
    buf.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
    // jmp *GOT_ENTRY(%rip) -> ff 25 <disp32>
    let disp = (got_entry_vaddr as i64) - (this_vaddr as i64 + 10);
    buf.extend_from_slice(&[0xff, 0x25]);
    buf.extend_from_slice(&(disp as i32).to_le_bytes());
    // push $index
    buf.push(0x68);
    buf.extend_from_slice(&(index as i32).to_le_bytes());
    // jmp PLT0
    let plt0_disp = (plt0_vaddr as i64) - (this_vaddr as i64 + 26);
    buf.push(0xe9);
    buf.extend_from_slice(&(plt0_disp as i32).to_le_bytes());
    // padding to 32 bytes
    while buf.len() < 32 {
        buf.push(0x66); // 2-byte NOP prefix
        buf.push(0x90);
    }
    buf.truncate(32);
    buf
}

/// GOT-resolved PLT 条目 (放入 .plt.got 节) 16 字节:
/// BIND_NOW 模式下使用, 直接 jmp *GOT 无 lazy stub.
///   jmp *GOT_ENTRY(%rip)        ; 6 bytes (ff 25 <disp32>)
///   nop * 10                    ; 10 bytes padding
///                              = 16 bytes
fn emit_plt_got_entry(this_vaddr: u64, got_entry_vaddr: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    // jmp *GOT_ENTRY(%rip) -> ff 25 <disp32>
    let disp = (got_entry_vaddr as i64) - (this_vaddr as i64 + 6);
    buf.extend_from_slice(&[0xff, 0x25]);
    buf.extend_from_slice(&(disp as i32).to_le_bytes());
    // padding to 16 bytes with NOPs
    while buf.len() < 16 {
        buf.push(0x90);
    }
    buf
}

/// PLT[0] 桩：32 字节
///   push *GOT[1](%rip)        ; 6 bytes
///   bnd jmp *GOT[2](%rip)     ; 6 bytes (bnd jmp: f2 ff 25 <disp32>)
///   nop dword ptr [rax+0]     ; 4 bytes (padding)
///   <padding>                 ; 16 bytes
///
/// CET/IBT 模式: 开头插入 endbr64 (4 bytes), padding 减少 4 bytes
fn emit_plt0(plt0_vaddr: u64, got1_vaddr: u64, got2_vaddr: u64, ibt: bool) -> Vec<u8> {
    let mut buf = Vec::with_capacity(32);
    if ibt {
        // endbr64 (CET 间接分支跟踪标记)
        buf.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
    }
    // push *GOT[1](%rip)
    let disp1 = (got1_vaddr as i64) - (plt0_vaddr as i64 + buf.len() as i64 + 6);
    buf.extend_from_slice(&[0xff, 0x35]);
    buf.extend_from_slice(&(disp1 as i32).to_le_bytes());
    // bnd jmp *GOT[2](%rip) = f2 ff 25 <disp32>
    let disp2 = (got2_vaddr as i64) - (plt0_vaddr as i64 + buf.len() as i64 + 6);
    buf.extend_from_slice(&[0xf2, 0xff, 0x25]);
    buf.extend_from_slice(&(disp2 as i32).to_le_bytes());
    // padding 到 32 字节
    while buf.len() < 32 {
        buf.extend_from_slice(&[0x66, 0x90]); // 2-byte nop
    }
    buf.truncate(32);
    buf
}

pub fn fill_plt_got(layout: &mut Layout) {
    // 复制 LinkOpts 出来 (避免后续借用)
    let opts = LinkOpts::from_env();
    fill_plt_got_with_opts(layout, &opts, &[]);
}

/// 带 LinkOpts 的版本：支持 BIND_NOW / RPATH / RUNPATH / DT_NEEDED。
/// pie_relas: PIE/Shared 下由 collect_pie_dynamic_relas 收集的
/// R_X86_64_RELATIVE 条目 (数据段指针初始化, 见 relocate.rs), 随 .rela.dyn
/// 一起输出, 由 ld.so 加载时写入 load_bias + addend。
pub fn fill_plt_got_with_opts(layout: &mut Layout, _opts: &LinkOpts, pie_relas: &[OutputRela]) {
    let plt_gots = PltGots::from_layout(layout);
    let n_plt = plt_gots.plt_funcs.len();
    let n_got_data = plt_gots.got_data.len();
    let ibt = _opts.ibt;

    // 找各节
    let (
        plt_idx,
        plt_sec_idx,
        plt_got_idx,
        gnu_prop_idx,
        got_plt_idx,
        got_idx,
        dynsym_idx,
        dynstr_idx,
        rel_plt_idx,
        rel_dyn_idx,
        dynamic_idx,
    ) = {
        let mut plt_i = None;
        let mut plt_sec_i = None;
        let mut plt_got_i = None;
        let mut gnu_prop_i = None;
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
                OutputSectionKind::PltSec => plt_sec_i = Some(i),
                OutputSectionKind::PltGot => plt_got_i = Some(i),
                OutputSectionKind::GnuProperty => gnu_prop_i = Some(i),
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
            plt_sec_i.unwrap_or(0),
            plt_got_i.unwrap_or(0),
            gnu_prop_i.unwrap_or(0),
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
        rela_plt_data
            .extend_from_slice(&((sym_idx as u64) << 32 | R_X86_64_JUMP_SLOT as u64).to_le_bytes());
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
            rela_dyn_data.extend_from_slice(
                &((sym_idx as u64) << 32 | R_X86_64_GLOB_DAT as u64).to_le_bytes(),
            );
            rela_dyn_data.extend_from_slice(&0i64.to_le_bytes());
        }
    }
    // PIE 数据段绝对重定位 → R_X86_64_RELATIVE: ld.so 加载时写入
    // load_bias + addend。addend = 符号最终 vaddr + 原 addend (第二遍调用时
    // vaddr 已由 recompute_section_vaddrs 更新, 两遍幂等: 第二遍覆盖第一遍)。
    for r in pie_relas {
        let r_offset = layout.sections[r.section as usize].vaddr + r.offset;
        let addend = (layout.symbols[r.sym as usize].vaddr as i64).wrapping_add(r.addend);
        rela_dyn_data.extend_from_slice(&r_offset.to_le_bytes());
        rela_dyn_data.extend_from_slice(&(R_X86_64_RELATIVE as u64).to_le_bytes());
        rela_dyn_data.extend_from_slice(&addend.to_le_bytes());
    }
    // 写 .dynstr
    let mut dynstr_data = vec![0u8];
    for name in &plt_gots.plt_funcs {
        for byte in name.bytes() {
            dynstr_data.push(byte);
        }
        dynstr_data.push(0);
    }
    for name in &plt_gots.got_data {
        for byte in name.bytes() {
            dynstr_data.push(byte);
        }
        dynstr_data.push(0);
    }
    // 生成 .plt (CET/IBT 模式下仅包含 PLT[0], 函数条目放入 .plt.sec)
    // BIND_NOW 模式下 .plt 也仅包含 PLT[0], 函数条目放入 .plt.got
    let bind_now = _opts.bind_now;
    let mut plt_data = Vec::with_capacity(32 + if ibt || bind_now { 0 } else { 16 * n_plt });
    let got1 = got_plt_vaddr + 8;
    let got2 = got_plt_vaddr + 16;
    plt_data.extend_from_slice(&emit_plt0(plt_vaddr, got1, got2, ibt));
    if !ibt && !bind_now {
        // 传统 PLT: 每个函数条目直接在 .plt 中
        for i in 0..n_plt {
            let this = plt_vaddr + plt_data.len() as u64;
            let got_entry = got_plt_vaddr + 24 + (i * 8) as u64;
            plt_data.extend_from_slice(&emit_plt_entry(plt_vaddr, this, got_entry, i as u64));
        }
    }
    // 生成 .plt.sec (CET/IBT 安全 PLT 条目, 非 BIND_NOW 模式)
    let mut plt_sec_data = Vec::new();
    let plt_sec_vaddr = if plt_sec_idx > 0 && ibt && !bind_now {
        layout.sections[plt_sec_idx].vaddr
    } else {
        0
    };
    if ibt && !bind_now && n_plt > 0 {
        for i in 0..n_plt {
            let this = plt_sec_vaddr + plt_sec_data.len() as u64;
            let got_entry = got_plt_vaddr + 24 + (i * 8) as u64;
            plt_sec_data
                .extend_from_slice(&emit_plt_sec_entry(plt_vaddr, this, got_entry, i as u64));
        }
    }
    // 生成 .plt.got (BIND_NOW 模式: GOT-resolved PLT, 直接 jmp *GOT 无 lazy stub)
    let mut plt_got_data = Vec::new();
    let plt_got_vaddr = if plt_got_idx > 0 && bind_now {
        layout.sections[plt_got_idx].vaddr
    } else {
        0
    };
    if bind_now && n_plt > 0 {
        for i in 0..n_plt {
            let this = plt_got_vaddr + plt_got_data.len() as u64;
            let got_entry = got_plt_vaddr + 24 + (i * 8) as u64;
            plt_got_data.extend_from_slice(&emit_plt_got_entry(this, got_entry));
        }
    }
    // 生成 .gnu.property (CET feature marking)
    // GNU_PROPERTY_X86_FEATURE_1_AND (0xc0000002):
    //   GNU_PROPERTY_X86_FEATURE_1_IBT (0x1)
    //   GNU_PROPERTY_X86_FEATURE_1_SHSTK (0x2)
    let mut gnu_prop_data = Vec::new();
    if ibt || _opts.shstk {
        // Note header: name_size=4, desc_size=4, type=NT_GNU_PROPERTY_TYPE_0(5)
        gnu_prop_data.extend_from_slice(b"GNU\0"); // name (4 bytes, aligned)
        gnu_prop_data.extend_from_slice(&5u32.to_le_bytes()); // type = NT_GNU_PROPERTY_TYPE_0
        gnu_prop_data.extend_from_slice(&16u32.to_le_bytes()); // data size (4+4+4+4)
                                                               // Property: GNU_PROPERTY_X86_FEATURE_1_AND (0xc0000002)
        gnu_prop_data.extend_from_slice(&0xc0000002u32.to_le_bytes()); // pr_type
        gnu_prop_data.extend_from_slice(&4u32.to_le_bytes()); // pr_datasz
        let mut feature_bits: u32 = 0;
        if ibt {
            feature_bits |= 0x1; // GNU_PROPERTY_X86_FEATURE_1_IBT
        }
        if _opts.shstk {
            feature_bits |= 0x2; // GNU_PROPERTY_X86_FEATURE_1_SHSTK
        }
        gnu_prop_data.extend_from_slice(&feature_bits.to_le_bytes());
        // padding to 8-byte align
        while gnu_prop_data.len() % 8 != 0 {
            gnu_prop_data.push(0);
        }
    }
    // 写 .dynamic
    let mut dynamic_data = Vec::new();
    let dynstr_size = dynstr_data.len() as u64;
    let rela_plt_size = rela_plt_data.len() as u64;
    let rela_dyn_size = rela_dyn_data.len() as u64;
    // DT_NEEDED (1) - 每个 DT_NEEDED 一个, value 指向 .dynstr 中的字符串
    // dcc 简化: 用 layout.soname / layout.needed
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
    // DT_FLAGS = 30 (BIND_NOW 等), DF_BIND_NOW = 0x8
    if _opts.bind_now {
        dynamic_data.extend_from_slice(&30u64.to_le_bytes()); // DT_FLAGS
        dynamic_data.extend_from_slice(&0x8u64.to_le_bytes()); // DF_BIND_NOW
    }
    // DT_FLAGS_1 = 0x6ffffffb (compat flag), DF_1_NOW = 0x1
    if _opts.bind_now {
        dynamic_data.extend_from_slice(&0x6ffffffbu64.to_le_bytes());
        dynamic_data.extend_from_slice(&0x1u64.to_le_bytes());
    }
    // DT_RPATH (23) / DT_RUNPATH (29) - 字符串在 .dynstr 末尾追加
    if let Some(rp) = &_opts.rpath {
        dynamic_data.extend_from_slice(&23u64.to_le_bytes());
        dynamic_data.extend_from_slice(&(dynstr_vaddr + dynstr_data.len() as u64).to_le_bytes());
        for b in rp.bytes() {
            dynstr_data.push(b);
        }
        dynstr_data.push(0);
    }
    if let Some(rp) = &_opts.runpath {
        dynamic_data.extend_from_slice(&29u64.to_le_bytes());
        dynamic_data.extend_from_slice(&(dynstr_vaddr + dynstr_data.len() as u64).to_le_bytes());
        for b in rp.bytes() {
            dynstr_data.push(b);
        }
        dynstr_data.push(0);
    }
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
    // 写回 .plt.sec (CET/IBT)
    if ibt && plt_sec_idx > 0 && !plt_sec_data.is_empty() {
        let plt_sec = &mut layout.sections[plt_sec_idx];
        plt_sec.data = plt_sec_data;
        plt_sec.size = plt_sec.data.len() as u64;
    }
    // 写回 .plt.got (BIND_NOW)
    if bind_now && plt_got_idx > 0 && !plt_got_data.is_empty() {
        let plt_got = &mut layout.sections[plt_got_idx];
        plt_got.data = plt_got_data;
        plt_got.size = plt_got.data.len() as u64;
    }
    // 写回 .gnu.property (CET feature marking)
    if gnu_prop_idx > 0 && !gnu_prop_data.is_empty() {
        let gnu_prop = &mut layout.sections[gnu_prop_idx];
        gnu_prop.data = gnu_prop_data;
        gnu_prop.size = gnu_prop.data.len() as u64;
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

    #[test]
    fn plt_sec_entry_starts_with_endbr64_and_correct_size() {
        let entry = emit_plt_sec_entry(0x401000, 0x402000, 0x403010, 0);
        assert_eq!(entry.len(), 32);
        // endbr64 = f3 0f 1e fa
        assert_eq!(&entry[0..4], &[0xf3, 0x0f, 0x1e, 0xfa]);
        // jmp *GOT(%rip) opcode at offset 4..6
        assert_eq!(&entry[4..6], &[0xff, 0x25]);
        // push $0 at offset 10
        assert_eq!(entry[10], 0x68);
        // jmp PLT0 at offset 15
        assert_eq!(entry[15], 0xe9);
    }

    #[test]
    fn plt0_ibt_starts_with_endbr64() {
        let plt0 = emit_plt0(0x401000, 0x402008, 0x402010, true);
        assert_eq!(plt0.len(), 32);
        assert_eq!(&plt0[0..4], &[0xf3, 0x0f, 0x1e, 0xfa]);
    }

    #[test]
    fn plt0_no_ibt_no_endbr64() {
        let plt0 = emit_plt0(0x401000, 0x402008, 0x402010, false);
        assert_eq!(plt0.len(), 32);
        assert_ne!(&plt0[0..4], &[0xf3, 0x0f, 0x1e, 0xfa]);
    }

    #[test]
    fn plt_got_entry_has_correct_size_and_opcode() {
        let entry = emit_plt_got_entry(0x401000, 0x402010);
        assert_eq!(entry.len(), 16);
        // jmp *GOT(%rip) = ff 25 <disp32>
        assert_eq!(&entry[0..2], &[0xff, 0x25]);
        // disp = 0x402010 - (0x401000 + 6) = 0x100a
        let disp = i32::from_le_bytes(entry[2..6].try_into().unwrap());
        assert_eq!(disp, 0x402010 - 0x401006);
        // rest is NOP padding
        for &b in &entry[6..16] {
            assert_eq!(b, 0x90);
        }
    }
}
