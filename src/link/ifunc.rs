// src/link/ifunc.rs - IFUNC (间接函数) 解析
//
// IFUNC 是 STT_GNU_IFUNC 类型的符号, 实际值在运行时由解析器 (resolver) 决定.
// 编译期:  引用 IFUNC 符号的代码使用一个 IRELATIVE 重定位, 指向 resolver 地址
// 链接期:  把 IFUNC 符号本身放入 .got (Irel_relatif), 记录 resolver 在 .rela.dyn
// 加载期:  动态链接器 (ld-linux) 调用 resolver, 把返回值写入 .got
//          之后所有引用此符号的代码通过 PLT/GOT 跳转
//
// dcc 实现简化方案 (静态可执行):
//   1. 链接期记录所有 STT_GNU_IFUNC 符号
//   2. 为每个 IFUNC 在 .rela.dyn 写一条 R_X86_64_IRELATIVE 重定位
//   3. 共享库加载时, 动态链接器执行 resolver 把最终函数地址写入 GOT 项
//   4. 引用方按普通函数调用处理 (PLT/GOT), 在静态可执行下会直接走 resolver
//
// 静态可执行模式 (ET_EXEC, `emit_dynamic == false`) 下, dcc 没有、也不可能「记录解析
// 关系然后交给别人执行」: 那条路上没有 ld.so, 而引用方是**提前固化**的直接 `call rel32`
// (没有 GOT 槽可改写), `apply_relocations` 里 R_X86_64_IRELATIVE 又是一个空操作分支
// (`relocate.rs` 的 `R_X86_64_GLOB_DAT | JUMP_SLOT | RELATIVE | IRELATIVE => {}`)。
// 所以静态路径必须自己把每个 IFUNC 定到一个具体实现体: 见 `bind_static_exec_ifuncs`。
// 要在静态产物里保留「运行时 CPUID 派发」, 缺的不是这几行, 而是三件成套的东西:
//   1. 引用方改成经 GOT 间接 (`call *(%rip)`), 而不是 `call` 直接绑 vaddr;
//   2. 启动期有一个重定位应用者 (glibc 静态链接靠 `.rela.iplt` + crt 的 `apply_irel`,
//      dcc 的内置 `_start` 桩在 `mod.rs::ensure_start_stub`, 目前只是调 `main`);
//   3. `.rela.dyn`/`.rela.iplt` 那套节在 ET_EXEC 里真的被写出来 (现在只有 PIE/Shared
//      走 `plt_got::fill_plt_got_with_opts`)。
// 这三件都在 `src/link/layout.rs` / `src/link/plt_got.rs` / `elf_exec.rs`, 不属于本模块,
// 因此静态路径选择「绑定 default 变体 + 无条件播报」, 而不是假装还能派发。

#![allow(dead_code)]

use std::collections::HashMap;

use super::layout::{Layout, OutputRela};
use crate::codegen::shared::elf::R_X86_64_IRELATIVE;

/// IFUNC 解析表：符号名 -> 解析器地址 (vaddr)
#[derive(Debug, Default, Clone)]
pub struct IfuncTable {
    /// 符号名 -> resolver 的输出 vaddr
    pub resolvers: HashMap<String, u64>,
    /// 符号名 -> IFUNC 自身 GLOB_DAT 重定位 (用于 .rela.dyn)
    pub glob_dats: Vec<(String, u64)>, // (name, offset in .got)
}

impl IfuncTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册 IFUNC：name 是 IFUNC 符号名，resolver_vaddr 是 resolver 函数地址
    pub fn register(&mut self, name: &str, resolver_vaddr: u64) {
        self.resolvers.insert(name.to_string(), resolver_vaddr);
    }

    /// 检查符号是否被注册为 IFUNC
    pub fn is_ifunc(&self, name: &str) -> bool {
        self.resolvers.contains_key(name)
    }
}

/// 从 Layout 中生成 IFUNC 解析表
/// 扫描所有 STT_GNU_IFUNC 类型 (kind=10) 符号
pub fn collect_ifuncs(layout: &Layout) -> IfuncTable {
    let mut table = IfuncTable::new();
    for sym in &layout.symbols {
        if sym.kind == 10
        /* STT_GNU_IFUNC */
        {
            // 找到 resolver (与 sym 同名, kind = STT_FUNC)
            // 简化: 用 sym 的 vaddr 作为 resolver
            let vaddr = sym.vaddr;
            table.register(&sym.name, vaddr);
        }
    }
    table
}

/// 从 .dcc.ifunc 节二进制载荷解析 ifunc 记录.
/// 格式: 每条记录 = u16 name_len + name bytes + u16 resolver_len + resolver bytes
/// 返回 Vec<(ifunc_name, resolver_name)>
pub fn parse_dcc_ifunc_section(data: &[u8]) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos + 2 <= data.len() {
        let name_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if pos + name_len > data.len() {
            break;
        }
        let name = std::str::from_utf8(&data[pos..pos + name_len])
            .unwrap_or("")
            .to_string();
        pos += name_len;
        if pos + 2 > data.len() {
            break;
        }
        let res_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if pos + res_len > data.len() {
            break;
        }
        let resolver = std::str::from_utf8(&data[pos..pos + res_len])
            .unwrap_or("")
            .to_string();
        pos += res_len;
        if !name.is_empty() && !resolver.is_empty() {
            entries.push((name, resolver));
        }
    }
    entries
}

/// 从 InputBundle 解析 .dcc.ifunc 节获取 (ifunc_name, resolver_name) 映射,
/// 再从 Layout 查找 resolver 的最终 vaddr, 注册到 IfuncTable.
/// 返回 IfuncTable 和新增的 IRELATIVE 重定位.
pub fn collect_from_bundle(
    bundle: &super::InputBundle,
    layout: &super::layout::Layout,
) -> (IfuncTable, Vec<super::layout::OutputRela>) {
    let mut table = IfuncTable::new();
    // 1. 从 bundle 的 .dcc.ifunc 节解析 (ifunc_name, resolver_name) 对
    let mut mappings: Vec<(String, String)> = Vec::new();
    for obj in &bundle.objects {
        for sec in &obj.sections {
            if sec.name == ".dcc.ifunc" {
                mappings.extend(parse_dcc_ifunc_section(&sec.data));
            }
        }
    }
    // 2. 对每个映射, 在 layout.symbols 查找 resolver 函数的 vaddr
    for (ifunc_name, resolver_name) in &mappings {
        let resolver_vaddr = layout
            .symbols
            .iter()
            .find(|s| s.name == *resolver_name)
            .map(|s| s.vaddr);
        if let Some(vaddr) = resolver_vaddr {
            table.register(ifunc_name, vaddr);
        }
    }
    let relas = gen_irelative_relas(&table);
    (table, relas)
}

/// 静态 ET_EXEC 里把每个 IFUNC 定到它的具体实现体。
///
/// 判据与动作：对 `layout.symbols` 里每一个 `STT_GNU_IFUNC` 符号 `f`，找同名变体
/// `f.default`（`target_clones("...,default")` 必然发它，codegen 侧的发形由
/// `src/codegen/x86_64/ifunc.rs` 保证）：
///   * 找到 → 把 `f` 的 `value`/`section`/`size` 改写成该变体的，并把类型降级成
///     `STT_FUNC`。静态产物里没有任何人会调用 resolver，留着 IFUNC 类型会让
///     `readelf -sW` 读出「有 IFUNC、却没有 IRELATIVE 来解析它」的假象；`STT_FUNC`
///     才是这条调用真正落到的地方。改写发生在 `apply_relocations` 之前，所以固化进
///     `.text` 的 `call rel32` 绑的就是这个变体地址。
///   * 找不到（用户手写 `__attribute__((ifunc("r")))`，resolver 是任意 C 函数，链接期
///     无法静态求值）→ 返回 `Err`，由调用方报错。**不**退化成「把调用绑到 resolver
///     体」：那样 `call f` 会返回一个函数指针而不是 `f` 的返回值，是静默的错程序
///     （实测纯 target_clones 程序被这样绑进 resolver 后 SIGSEGV）。
///
/// 成功时返回 `(ifunc 名, 被绑到的变体名)` 列表，调用方**无条件**打印 —— 这是
/// 「静态路径放弃了 CPUID 派发」这件事的唯一对外说明口。
pub fn bind_static_exec_ifuncs(
    layout: &mut Layout,
) -> Result<Vec<(String, String)>, Vec<String>> {
    use crate::codegen::shared::elf::{STT_FUNC, STT_GNU_IFUNC};

    // 变体表：`<base>.default` -> (section, value, size, vaddr)。一次扫描，避免每个 IFUNC 都 O(n) 查。
    // `vaddr` 直接搬目标的，不重算：它已经由 `build_symbols` 按 `section.vaddr + value`
    // 定好，而 symtab 的 `st_value` 写的就是它（`elf_exec.rs`），`apply_relocations` 则用
    // `sections[sym.section].vaddr + sym.value` —— 两者必须同时搬，否则符号表与指令不一致。
    let mut defaults: HashMap<String, (u16, u64, u64, u64)> = HashMap::new();
    for s in &layout.symbols {
        if let Some(base) = s.name.strip_suffix(".default") {
            if s.ty == STT_FUNC || s.kind == STT_FUNC {
                defaults.insert(base.to_string(), (s.section, s.value, s.size, s.vaddr));
            }
        }
    }

    let mut bound = Vec::new();
    let mut unresolvable = Vec::new();
    for i in 0..layout.symbols.len() {
        let is_ifunc = layout.symbols[i].kind == STT_GNU_IFUNC
            || layout.symbols[i].ty == STT_GNU_IFUNC;
        if !is_ifunc {
            continue;
        }
        let name = layout.symbols[i].name.clone();
        if name.is_empty() {
            continue;
        }
        match defaults.get(name.as_str()) {
            Some(&(section, value, size, vaddr)) => {
                let sym = &mut layout.symbols[i];
                sym.section = section;
                sym.value = value;
                sym.size = size;
                sym.vaddr = vaddr;
                sym.ty = STT_FUNC;
                sym.kind = STT_FUNC;
                bound.push((name.clone(), format!("{name}.default")));
            },
            None => unresolvable.push(name),
        }
    }
    if unresolvable.is_empty() {
        Ok(bound)
    } else {
        unresolvable.sort();
        Err(unresolvable)
    }
}

/// 为 IFUNC 生成 .rela.dyn 记录 (R_X86_64_IRELATIVE)
/// 动态链接器会在加载时调用 resolver 并把返回值写入对应 GOT 项
pub fn gen_irelative_relas(table: &IfuncTable) -> Vec<OutputRela> {
    let mut out = Vec::new();
    for (name, &resolver_vaddr) in &table.resolvers {
        out.push(OutputRela::new(0, 0, 0, R_X86_64_IRELATIVE, resolver_vaddr as i64));
        if let Some(last) = out.last_mut() {
            last._name = format!("__ifunc_reloc_{}", name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::shared::elf::STT_NOTYPE;
    use crate::link::layout::OutputSymbol;

    #[test]
    fn test_register_and_lookup() {
        let mut t = IfuncTable::new();
        t.register("memcpy_ifunc", 0x1000);
        assert!(t.is_ifunc("memcpy_ifunc"));
        assert!(!t.is_ifunc("memcpy"));
        assert_eq!(t.resolvers.get("memcpy_ifunc"), Some(&0x1000));
    }

    #[test]
    fn test_irelative_relas_count() {
        let mut t = IfuncTable::new();
        t.register("a", 0x1000);
        t.register("b", 0x2000);
        let relas = gen_irelative_relas(&t);
        assert_eq!(relas.len(), 2);
        for r in &relas {
            assert_eq!(r.rel_type, R_X86_64_IRELATIVE);
        }
    }

    /// 编码辅助: 按 .dcc.ifunc 节格式序列化 (name, resolver) 对
    fn encode_ifunc_section(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut data = Vec::new();
        for (name, resolver) in entries {
            let n = name.as_bytes();
            let r = resolver.as_bytes();
            data.extend_from_slice(&(n.len() as u16).to_le_bytes());
            data.extend_from_slice(n);
            data.extend_from_slice(&(r.len() as u16).to_le_bytes());
            data.extend_from_slice(r);
        }
        data
    }

    #[test]
    fn test_parse_dcc_ifunc_section_roundtrip() {
        let entries = vec![("memcpy", "resolve_memcpy"), ("strlen", "resolve_strlen")];
        let data = encode_ifunc_section(&entries);
        let parsed = parse_dcc_ifunc_section(&data);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], ("memcpy".to_string(), "resolve_memcpy".to_string()));
        assert_eq!(parsed[1], ("strlen".to_string(), "resolve_strlen".to_string()));
    }

    #[test]
    fn test_parse_dcc_ifunc_section_empty() {
        let parsed = parse_dcc_ifunc_section(&[]);
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_parse_dcc_ifunc_section_truncated() {
        // name_len 声明 100 字节但数据不足, 应安全截断
        let data = [100u8, 0, b'a', b'b'];
        let parsed = parse_dcc_ifunc_section(&data);
        assert!(parsed.is_empty());
    }

    /// 构造一个最小 Layout: 一个 .text 输出节 + 调用方给的符号。
    /// 只够 `bind_static_exec_ifuncs` 用 (它只看 symbols 的 name/ty/kind/value/size/vaddr
    /// 以及 sections[i].vaddr), 不参与地址分配。
    fn layout_with_symbols(symbols: Vec<OutputSymbol>) -> Layout {
        use crate::codegen::shared::elf::{SHT_PROGBITS, SHF_ALLOC, SHF_EXECINSTR};
        use crate::link::layout::{OutputSection, OutputSectionKind};
        let text = OutputSection {
            kind: OutputSectionKind::Text,
            name: ".text".to_string(),
            sh_name_off: 0,
            ty: SHT_PROGBITS,
            flags: SHF_ALLOC | SHF_EXECINSTR,
            align: 16,
            data: Vec::new(),
            size: 0x1000,
            entsize: 0,
            output_index: 0,
            load_segment: 0,
            vaddr: 0x400160,
            file_off: 0x160,
            input_offsets: Vec::new(),
            input_sections: Vec::new(),
        };
        Layout {
            is_pie: false,
            is_shared: false,
            arch: crate::debug::ArchKind::X86_64,
            base_addr: 0x400000,
            file_base: 0x160,
            sections: vec![text],
            segments: vec![],
            symbols,
            entry: 0,
            has_symtab: true,
            has_dynamic: false,
            n_locals: 0,
            output_relas: vec![],
            needed: vec![],
            dynamic_linker: None,
            comdat_groups: vec![],
        }
    }

    fn sym(name: &str, ty: u8, value: u64, size: u64) -> OutputSymbol {
        use crate::codegen::shared::elf::{STB_GLOBAL, STB_LOCAL, STV_DEFAULT};
        OutputSymbol {
            name: name.to_string(),
            value,
            vaddr: 0x400160 + value,
            size,
            bind: if name.starts_with("__") || name.contains('.') {
                STB_LOCAL
            } else {
                STB_GLOBAL
            },
            ty,
            kind: ty,
            visibility: STV_DEFAULT,
            section: 0,
            shndx_if_section: 0,
        }
    }

    /// 静态 ET_EXEC: IFUNC 被绑到 `<base>.default` 变体, 类型降为 STT_FUNC,
    /// 并返回可播报的 (ifunc, 变体) 对 —— 不留在 resolver 体上 (那是 SIGSEGV 那个形状)。
    #[test]
    fn bind_static_exec_ifuncs_binds_to_default_variant() {
        use crate::codegen::shared::elf::{STT_FUNC, STT_GNU_IFUNC};
        let mut layout = layout_with_symbols(vec![
            sym("", STT_NOTYPE, 0, 0),
            sym("compute", STT_GNU_IFUNC, 0x2e0, 41), // resolver 体地址 (新形状)
            sym("compute.resolver", STT_FUNC, 0x2e0, 41),
            sym("compute.sse4_2", STT_FUNC, 0x0, 100),
            sym("compute.default", STT_FUNC, 0x68, 100),
        ]);
        let bound = bind_static_exec_ifuncs(&mut layout).expect("default 变体存在, 应可绑");
        assert_eq!(bound, vec![("compute".to_string(), "compute.default".to_string())]);
        let compute = layout
            .symbols
            .iter()
            .find(|s| s.name == "compute")
            .unwrap();
        let default = layout
            .symbols
            .iter()
            .find(|s| s.name == "compute.default")
            .unwrap();
        assert_eq!(compute.value, default.value);
        assert_eq!(compute.vaddr, default.vaddr);
        assert_eq!(compute.size, default.size);
        assert_eq!(compute.section, default.section);
        assert_eq!(compute.ty, STT_FUNC, "静态产物里不该再剩 IFUNC 类型");
        assert_eq!(compute.kind, STT_FUNC);
        // resolver 体本身不动 (它仍在 .text 里, 只是没人调用)
        let resolver = layout
            .symbols
            .iter()
            .find(|s| s.name == "compute.resolver")
            .unwrap();
        assert_eq!(resolver.value, 0x2e0);
    }

    /// 静态 ET_EXEC: 用户手写 `__attribute__((ifunc("r")))` 没有 `default` 变体时
    /// 必须报错, 不能静默把调用绑到 resolver 体 (那会返回函数指针当返回值)。
    #[test]
    fn bind_static_exec_ifuncs_errors_without_default_variant() {
        use crate::codegen::shared::elf::{STT_FUNC, STT_GNU_IFUNC};
        let mut layout = layout_with_symbols(vec![
            sym("", STT_NOTYPE, 0, 0),
            sym("ifunc_func", STT_GNU_IFUNC, 0, 54),
            sym("resolve_ifunc", STT_FUNC, 0, 54),
            sym("default_impl", STT_FUNC, 0x38, 60),
        ]);
        let err = bind_static_exec_ifuncs(&mut layout)
            .err()
            .expect("无 default 变体时应 Err, 不得静默绑 resolver");
        assert_eq!(err, vec!["ifunc_func".to_string()]);
        // 符号未被就地改写
        assert_eq!(layout.symbols[1].kind, STT_GNU_IFUNC);
    }

    /// 没有 IFUNC 符号时 (大多数程序) 本步为空操作, 不能误伤同名前缀的符号。
    #[test]
    fn bind_static_exec_ifuncs_noop_without_ifunc() {
        use crate::codegen::shared::elf::STT_FUNC;
        let mut layout = layout_with_symbols(vec![
            sym("", STT_NOTYPE, 0, 0),
            sym("main", STT_FUNC, 0, 42),
            sym("foo.default", STT_FUNC, 100, 8),
        ]);
        assert_eq!(bind_static_exec_ifuncs(&mut layout).unwrap(), vec![]);
        assert_eq!(layout.symbols[1].value, 0);
        assert_eq!(layout.symbols[2].value, 100);
    }

    #[test]
    fn test_parse_dcc_ifunc_section_single() {
        let data = encode_ifunc_section(&[("foo", "bar")]);
        let parsed = parse_dcc_ifunc_section(&data);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], ("foo".to_string(), "bar".to_string()));
    }
}
