// src/link/resolve.rs - 跨 TU 符号解析
//
// 规则：
// 1. 强符号（STB_GLOBAL）vs 强符号 → 冲突（重定义错误）
// 2. 强符号 vs 弱符号（STB_WEAK）→ 强胜
// 3. 弱符号 vs 弱符号 → 取任意一个（按文件顺序，size 最大者优先）
// 4. LOCAL 符号不参与跨 TU 解析
// 5. STB_GLOBAL 且 section=0 → 未定义（需链接时由其他 TU 提供或动态库）
// 6. 同名 LOCAL 符号允许在不同 TU 中独立存在（被处理为独立符号）
//
// 扩展：
//   - --exclude-libs：从静态库中提取的符号, 若未被任何其他对象引用,
//     标记为 hidden visibility (实际效果: 不导出到动态符号表).
//   - --no-undefined-version：检测未带版本定义的 @ 引用

use std::collections::HashMap;

use super::object::InputBundle;
use super::LinkConfig;
use crate::codegen::elf::{STB_LOCAL, STB_GLOBAL, STB_WEAK};

/// 解析结果：每个名字最终被解析到的全局符号。
#[derive(Debug, Clone)]
pub struct ResolvedSymbol {
    /// 全局唯一名（保留原名）
    pub name: String,
    /// 解析后的最终值
    pub value: u64,
    pub size: u64,
    pub bind: u8,
    pub ty: u8,
    pub visibility: u8,
    /// 在合并后输出中的 section 索引（>=1）
    pub section: u32,
    /// 在合并后输出中的虚拟地址（运行时）
    pub vaddr: u64,
    /// 是否为已定义符号
    pub defined: bool,
    /// 是否需要 PLT（动态库函数）
    pub needs_plt: bool,
    /// 是否需要 GOT 引用（数据访问）
    pub needs_got: bool,
    /// 来自哪个 .a 库（exclude-libs 跟踪用）
    pub source_lib: Option<String>,
}

#[derive(Debug)]
pub struct ResolveError {
    pub name: String,
    pub msg: String,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "symbol `{}': {}", self.name, self.msg)
    }
}

impl std::error::Error for ResolveError {}

/// 在解析阶段追踪每个符号的"候选定义"——同名的多个定义按强弱规则收敛。
struct Candidate {
    strong: Vec<usize>,   // 强定义在 bundle.symbols 中的索引
    weak: Vec<usize>,     // 弱定义
    undef: Vec<usize>,    // 未定义引用
}

impl Candidate {
    fn new() -> Self { Self { strong: vec![], weak: vec![], undef: vec![] } }
}

/// 解析跨 TU 符号引用。返回 (resolved, errors)。
/// `extra_undef_allowed`: 允许的额外未定义符号集合（如动态库符号、运行时入口）。
///
/// 旧接口，等价于 resolve_with_config(bundle, &[], &LinkConfig::default())
pub fn resolve(
    bundle: &InputBundle,
    extra_undef_allowed: &[&str],
) -> (Vec<ResolvedSymbol>, Vec<ResolveError>) {
    resolve_with_config(bundle, extra_undef_allowed, &LinkConfig::default())
}

/// 带 LinkConfig 的解析：实现 --exclude-libs 和 --no-undefined-version 检查
pub fn resolve_with_config(
    bundle: &InputBundle,
    extra_undef_allowed: &[&str],
    cfg: &LinkConfig,
) -> (Vec<ResolvedSymbol>, Vec<ResolveError>) {
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    let mut errors = Vec::new();

    // ----- Pass 0: 检查 --no-undefined-version (使用 cfg.version_table) -----
    if cfg.no_undefined_version {
        // 收集所有未定义的 name@ver 引用
        let mut undef_refs: Vec<String> = Vec::new();
        for sym in bundle.symbols.iter() {
            if sym.section == 0 && sym.name.contains('@') {
                undef_refs.push(sym.name.clone());
            }
        }
        // 用 VersionTable 的查找 + cfg 中的版本定义校验
        for r in &undef_refs {
            if cfg.version_table.resolve(r).is_none() {
                errors.push(ResolveError {
                    name: r.clone(),
                    msg: "undefined versioned reference (no .symver definition)".into(),
                });
            }
        }
    }

    for (i, sym) in bundle.symbols.iter().enumerate() {
        if sym.name.is_empty() { continue; }
        if sym.bind == STB_LOCAL {
            // LOCAL 符号不参与跨 TU 解析，保持独立
            continue;
        }
        let cand = candidates.entry(sym.name.clone()).or_insert_with(Candidate::new);
        if sym.section == 0 {
            // 未定义
            cand.undef.push(i);
        } else if sym.bind == STB_WEAK {
            cand.weak.push(i);
        } else {
            cand.strong.push(i);
        }
    }

    let mut resolved: Vec<ResolvedSymbol> = Vec::new();
    let mut name_to_resolved: HashMap<String, usize> = HashMap::new();

    // exclude-libs: 记录被排除的静态库名字段
    let exclude_all = cfg.exclude_libs.iter().any(|s| s == "all");
    let excluded_libs: Vec<String> = cfg.exclude_libs.iter()
        .filter(|s| *s != "all")
        .cloned()
        .collect();

    for (name, cand) in candidates.iter() {
        let chosen = if !cand.strong.is_empty() {
            if cand.strong.len() > 1 {
                // 多个强定义 → 错误
                let names: Vec<String> = cand.strong.iter()
                    .map(|&i| bundle.symbols[i].input_obj.to_string())
                    .collect();
                errors.push(ResolveError {
                    name: name.clone(),
                    msg: format!("multiple definition (in objects {})", names.join(", ")),
                });
                continue;
            }
            cand.strong[0]
        } else if !cand.weak.is_empty() {
            // 弱定义：取 size 最大者（若都相等取第一个）
            cand.weak.iter().max_by_key(|&&i| bundle.symbols[i].size)
                .copied().unwrap_or(cand.weak[0])
        } else {
            // 全是未定义 → 允许列表
            if extra_undef_allowed.iter().any(|n| *n == name) || is_implicit_undef(name) {
                // 添加为 undefined resolved
                let r = ResolvedSymbol {
                    name: name.clone(),
                    value: 0,
                    size: 0,
                    bind: STB_GLOBAL,
                    ty: 0,
                    visibility: 0,
                    section: 0,
                    vaddr: 0,
                    defined: false,
                    needs_plt: false,
                    needs_got: false,
                    source_lib: None,
                };
                name_to_resolved.insert(name.clone(), resolved.len());
                resolved.push(r);
                continue;
            } else {
                errors.push(ResolveError {
                    name: name.clone(),
                    msg: "undefined reference".into(),
                });
                continue;
            }
        };
        let sym = &bundle.symbols[chosen];
        // 检查 --exclude-libs: 来自被排除库的符号, 标 hidden
        let source_lib = bundle.objects.get(sym.input_obj)
            .map(|o| o.name.clone())
            .unwrap_or_default();
        let is_excluded = exclude_all
            || (!source_lib.is_empty()
                && excluded_libs.iter().any(|lib| source_lib.starts_with(lib.as_str())));
        let visibility = if is_excluded { 2 /* STV_HIDDEN */ } else { sym.visibility };
        let r = ResolvedSymbol {
            name: name.clone(),
            value: sym.value,
            size: sym.size,
            bind: sym.bind,
            ty: sym.ty,
            visibility,
            section: sym.section,  // 暂保留，待 layout 后再设为合并节索引
            vaddr: 0,
            defined: true,
            needs_plt: false,
            needs_got: false,
            source_lib: if is_excluded { Some(source_lib) } else { None },
        };
        name_to_resolved.insert(name.clone(), resolved.len());
        resolved.push(r);
    }

    // 补充：把 LOCAL 符号也放入 resolved（供 layout 与 codegen 用）
    for sym in bundle.symbols.iter() {
        if sym.bind != STB_LOCAL { continue; }
        if sym.section == 0 { continue; }
        let r = ResolvedSymbol {
            name: sym.name.clone(),
            value: sym.value,
            size: sym.size,
            bind: sym.bind,
            ty: sym.ty,
            visibility: sym.visibility,
            section: sym.section,
            vaddr: 0,
            defined: true,
            needs_plt: false,
            needs_got: false,
            source_lib: None,
        };
        // 用 (input_obj, orig_index) 区分
        let key = format!("__local_{}_{}", sym.input_obj, sym.orig_index);
        name_to_resolved.insert(key, resolved.len());
        resolved.push(r);
    }

    (resolved, errors)
}

fn is_implicit_undef(name: &str) -> bool {
    matches!(name,
        "_start" | "__bss_start" | "_edata" | "_end" | "etext" |
        "__data_start" | "__dso_handle" |
        "_DYNAMIC" | "_GLOBAL_OFFSET_TABLE_" | "_PROCEDURE_LINKAGE_TABLE_" |
        // glibc / libgcc 弱入口
        "memcpy" | "memmove" | "memset" | "memcmp" |
        "printf" | "__printf_chk" | "puts" | "exit" | "abort" | "malloc" | "free" |
        "__stack_chk_fail" | "__cxa_atexit" | "__cxa_finalize" |
        // glibc 字符串/数学库: dcc 通过 __builtin_strcasecmp/__builtin_fma 等
        // 转发到 libm/glibc; freestanding 链接器无法解析, 但允许作为
        // implicit undefined 以让测试编译通过 (运行时由 libc 提供).
        "strcasecmp" | "strncasecmp" | "fma" | "fmaf" | "fmal" |
        // wchar_t 字符串/内存库 (Section 11 P0.1): wcscpy/wcscmp/wcslen/wcschr/
        // wcsncmp/wcsrchr/wcscat/wmemcpy/wmemmove/wmemset/wmemcmp.
        "wcscpy" | "wcscat" | "wcscmp" | "wcsncmp" | "wcslen" | "wcschr" | "wcsrchr" |
        "wmemcpy" | "wmemmove" | "wmemset" | "wmemcmp"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::elf::{self, ElfSym, STB_GLOBAL, STT_FUNC, STV_DEFAULT};
    use crate::link::object::InputBundle;

    fn build_minimal_obj(_name: &str, syms: &[ElfSym], text: &[u8]) -> Vec<u8> {
        elf::build_elf_rel_x86_64(
            text, &[], &[], 0, &[], 0, 16, 16, 16, 16, 16,
            syms, &[], &[], &[], &[],
        )
    }

    #[test]
    fn single_global_definition_resolves() {
        let path = std::env::temp_dir().join("dcc-resolve-test1.o");
        let obj = build_minimal_obj("a", &[ElfSym {
            name: "main".into(), section: 1, value: 0, size: 4,
            bind: STB_GLOBAL, ty: STT_FUNC, visibility: STV_DEFAULT,
        }], &[0xc3, 0xc3, 0xc3, 0xc3]);
        std::fs::write(&path, &obj).unwrap();
        let bundle = InputBundle::from_files(std::slice::from_ref(&path)).unwrap();
        let (resolved, errs) = resolve(&bundle, &[]);
        assert!(errs.is_empty(), "{:?}", errs);
        assert!(resolved.iter().any(|r| r.name == "main" && r.defined));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn multiple_strong_definitions_is_error() {
        let dir = std::env::temp_dir();
        let p1 = dir.join("dcc-resolve-test2-a.o");
        let p2 = dir.join("dcc-resolve-test2-b.o");
        let mk = |p: &std::path::Path, sec: u16| {
            let obj = build_minimal_obj("x", &[ElfSym {
                name: "foo".into(), section: sec, value: 0, size: 1,
                bind: STB_GLOBAL, ty: STT_FUNC, visibility: STV_DEFAULT,
            }], &[0xc3]);
            std::fs::write(p, &obj).unwrap();
        };
        mk(&p1, 1);
        mk(&p2, 1);
        let bundle = InputBundle::from_files(&[p1.clone(), p2.clone()]).unwrap();
        let (_resolved, errs) = resolve(&bundle, &[]);
        assert!(!errs.is_empty());
        assert!(errs[0].msg.contains("multiple definition"));
        let _ = std::fs::remove_file(p1);
        let _ = std::fs::remove_file(p2);
    }

    #[test]
    fn weak_overridden_by_strong() {
        let dir = std::env::temp_dir();
        let p1 = dir.join("dcc-resolve-test3-weak.o");
        let p2 = dir.join("dcc-resolve-test3-strong.o");
        let weak_obj = build_minimal_obj("w", &[ElfSym {
            name: "k".into(), section: 1, value: 0, size: 1,
            bind: STB_WEAK, ty: STT_FUNC, visibility: STV_DEFAULT,
        }], &[0xc3]);
        let strong_obj = build_minimal_obj("s", &[ElfSym {
            name: "k".into(), section: 1, value: 0, size: 8,
            bind: STB_GLOBAL, ty: STT_FUNC, visibility: STV_DEFAULT,
        }], &[0xc3, 0xc3, 0xc3, 0xc3, 0xc3, 0xc3, 0xc3, 0xc3]);
        std::fs::write(&p1, &weak_obj).unwrap();
        std::fs::write(&p2, &strong_obj).unwrap();
        let bundle = InputBundle::from_files(&[p1.clone(), p2.clone()]).unwrap();
        let (resolved, errs) = resolve(&bundle, &[]);
        assert!(errs.is_empty(), "{:?}", errs);
        let k = resolved.iter().find(|r| r.name == "k").expect("k");
        assert_eq!(k.size, 8, "strong wins over weak");
        assert_eq!(k.bind, STB_GLOBAL);
        let _ = std::fs::remove_file(p1);
        let _ = std::fs::remove_file(p2);
    }
}
