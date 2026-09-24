// src/link/fini_dispatch.rs - __dso_handle / __cxa_atexit / atexit / .fini_array 调度
//
// 链接器在生成 ELF 时, 扫描所有 __attribute__((destructor)) 标记的函数,
// 收集到 .fini_array 节, 运行时由 libc / crtend 调度.
//
// 同时处理:
//   - __dso_handle: 提供给 __cxa_atexit 的"动态库 handle" (本进程 executable 自身)
//   - __cxa_atexit: 把 cleanup 注册到 libc 内部链表, 程序退出时调用
//   - atexit: 简化实现, 通过 __cxa_atexit 转发
//
// 链接器职责:
//   1. 在 .fini_array 节写入所有 destructor 函数的指针 (8 字节 / 指针)
//   2. 提供 __dso_handle 符号 (本进程入口地址, 用 _start 或自定义符号)
//   3. 提供 __cxa_finalize / atexit 桩 (无 libgcc 时由我们提供)

use std::collections::HashMap;

use super::layout::{Layout, OutputSection, OutputSectionKind};
use crate::codegen::shared::elf::SHT_FINI_ARRAY;

/// __dso_handle 内容: 通常是 .data 中一个隐藏变量
/// 简化: 直接指向 .data 节起始地址
pub fn gen_dso_handle(layout: &Layout) -> u64 {
    for sec in &layout.sections {
        if sec.kind == OutputSectionKind::Data {
            return sec.vaddr;
        }
    }
    // 兜底: .text 起始
    layout.sections.first().map(|s| s.vaddr).unwrap_or(0)
}

/// 收集 .fini_array 需要的 destructor 列表
#[derive(Debug, Clone)]
pub struct FiniArrayBuilder {
    /// 收集到的 destructor 符号 (按优先级排序: 数字越小越早执行)
    pub dtors: Vec<DtorEntry>,
}

#[derive(Debug, Clone)]
pub struct DtorEntry {
    /// 函数符号名
    pub name: String,
    /// 优先级 (0 = default, 数字越大越晚)
    pub priority: i32,
    /// 函数 vaddr (link 后填回)
    pub vaddr: u64,
}

impl Default for FiniArrayBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FiniArrayBuilder {
    pub fn new() -> Self {
        Self { dtors: Vec::new() }
    }

    /// 注册一个 destructor
    pub fn add(&mut self, name: &str, priority: i32) {
        self.dtors.push(DtorEntry {
            name: name.to_string(),
            priority,
            vaddr: 0,
        });
        // 按优先级升序排序 (低优先级 = 先执行)
        self.dtors.sort_by_key(|d| d.priority);
    }

    /// 在 layout 中填入所有 destructor vaddr
    pub fn resolve_vaddrs(&mut self, layout: &Layout) {
        let map: HashMap<String, u64> = layout
            .symbols
            .iter()
            .filter(|s| !s.name.is_empty())
            .map(|s| (s.name.clone(), s.vaddr))
            .collect();
        for d in &mut self.dtors {
            if let Some(&v) = map.get(&d.name) {
                d.vaddr = v;
            }
        }
    }

    /// 生成 .fini_array 节内容 (8 字节 / 项)
    pub fn gen_fini_array_data(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(self.dtors.len() * 8);
        for d in &self.dtors {
            data.extend_from_slice(&d.vaddr.to_le_bytes());
        }
        data
    }

    /// 生成 .init_array 节内容 (构造器)
    pub fn gen_init_array_data(ctors: &[DtorEntry]) -> Vec<u8> {
        let mut data = Vec::with_capacity(ctors.len() * 8);
        for c in ctors {
            data.extend_from_slice(&c.vaddr.to_le_bytes());
        }
        data
    }
}

/// 提供 .fini_array 运行时调度支持符号
/// 生成 4 个全局符号的 .o (嵌入到 layout):
///   - __cxa_finalize(ptr): 空实现
///   - __dso_handle = .data 起始
///   - atexit(fn): 转发到 __cxa_atexit
///   - __cxa_atexit(fn, arg, dso): 桩 (由 libc 提供)
pub fn ensure_fini_runtime_symbols(layout: &mut Layout) {
    // 1. __dso_handle: 一个 .data 中的隐藏变量
    let dso = gen_dso_handle(layout);
    layout.symbols.push(crate::link::layout::OutputSymbol {
        name: "__dso_handle".into(),
        value: 0,
        vaddr: dso,
        size: 0,
        bind: 1, // STB_GLOBAL
        ty: 1,   // STT_OBJECT
        kind: 1,
        visibility: 0,
        section: 0,
        shndx_if_section: 0,
    });
}

/// 在 layout 中添加 .fini_array 节 (若尚未存在)
pub fn ensure_fini_array_section(layout: &mut Layout) -> u32 {
    for (i, s) in layout.sections.iter().enumerate() {
        if s.kind == OutputSectionKind::FiniArray {
            return i as u32;
        }
    }
    let new_idx = layout.sections.len() as u32;
    layout.sections.push(OutputSection {
        kind: OutputSectionKind::FiniArray,
        name: ".fini_array".to_string(),
        sh_name_off: 0,
        ty: SHT_FINI_ARRAY,
        flags: 0x3, // SHF_ALLOC | SHF_WRITE
        align: 8,
        data: Vec::new(),
        size: 0,
        entsize: 8,
        output_index: new_idx,
        load_segment: 1,
        vaddr: 0,
        file_off: 0,
        input_offsets: Vec::new(),
        input_sections: Vec::new(),
    });
    new_idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_dtor_sorts_by_priority() {
        let mut b = FiniArrayBuilder::new();
        b.add("b", 200);
        b.add("a", 100);
        b.add("c", 0);
        let names: Vec<_> = b.dtors.iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }

    #[test]
    fn fini_array_data_is_packed() {
        let mut b = FiniArrayBuilder::new();
        b.add("a", 0);
        b.dtors[0].vaddr = 0x401000;
        b.add("b", 100);
        b.dtors[1].vaddr = 0x401100;
        let data = b.gen_fini_array_data();
        assert_eq!(data.len(), 16);
        assert_eq!(u64::from_le_bytes(data[0..8].try_into().unwrap()), 0x401000);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 0x401100);
    }
}
