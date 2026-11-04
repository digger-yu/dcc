// src/incremental.rs - 增量编译: 源文件哈希 + 目标文件更新检查
//
// 第一性原理: 编译是纯函数 (源 + 标志 -> 目标). 若输入未变, 输出无需重算.
// 实现: 对 (源内容, 相关编译标志) 计算哈希, 与缓存对比; 命中则跳过编译.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::cli::Config;

/// 增量编译缓存管理器.
///
/// 缓存目录: `.dcc-cache/` (与 Make 的 `.d` 文件同级).
/// 每个源文件对应一个缓存条目, 文件名 = 源路径的哈希.
/// 缓存内容 = 当前编译的指纹 (源内容 + 影响输出的标志).
pub struct IncrementalCache {
    /// 缓存根目录
    cache_dir: PathBuf,
    /// 是否 verbose 模式
    verbose: bool,
}

/// 缓存查询结果
pub enum CacheResult {
    /// 缓存命中: 输出文件已是最新, 无需重编译
    Hit,
    /// 缓存未命中: 需要重编译, 返回缓存条目路径供后续更新
    Miss { entry_path: PathBuf },
}

impl IncrementalCache {
    /// 创建增量编译缓存. 缓存目录为 `.dcc-cache/`.
    pub fn new(verbose: bool) -> Self {
        Self {
            cache_dir: PathBuf::from(".dcc-cache"),
            verbose,
        }
    }

    /// 创建使用自定义缓存目录的增量编译缓存 (测试用).
    #[cfg(test)]
    fn with_cache_dir(cache_dir: PathBuf, verbose: bool) -> Self {
        Self { cache_dir, verbose }
    }

    /// 检查源文件是否需要重编译.
    ///
    /// 判定逻辑:
    /// 1. 计算当前 (源内容 + 编译标志) 的指纹
    /// 2. 若输出文件不存在 → Miss (需要编译)
    /// 3. 若缓存条目不存在 → Miss (首次编译)
    /// 4. 若缓存中的指纹与当前不同 → Miss (源或标志变了)
    /// 5. 否则 → Hit (跳过编译)
    pub fn check(&self, cfg: &Config, input: &Path) -> CacheResult {
        let entry_path = self.entry_path(input);
        let output = &cfg.output;

        // 输出文件不存在 → 必须编译
        if !output.exists() {
            if self.verbose {
                eprintln!("dcc: incremental: {} -> output missing, recompiling", input.display());
            }
            return CacheResult::Miss { entry_path };
        }

        // 计算当前指纹
        let current_fingerprint = match self.compute_fingerprint(cfg, input) {
            Some(fp) => fp,
            None => {
                // 无法计算指纹 (如读源失败) → 保守重编译
                if self.verbose {
                    eprintln!(
                        "dcc: incremental: {} -> cannot compute fingerprint, recompiling",
                        input.display()
                    );
                }
                return CacheResult::Miss { entry_path };
            },
        };

        // 读缓存条目
        let cached_fingerprint = match fs::read_to_string(&entry_path) {
            Ok(s) => s,
            Err(_) => {
                if self.verbose {
                    eprintln!("dcc: incremental: {} -> no cache entry, compiling", input.display());
                }
                return CacheResult::Miss { entry_path };
            },
        };

        // 对比指纹
        if current_fingerprint == cached_fingerprint {
            if self.verbose {
                eprintln!("dcc: incremental: {} -> up to date (skipped)", input.display());
            }
            CacheResult::Hit
        } else {
            if self.verbose {
                eprintln!(
                    "dcc: incremental: {} -> fingerprint changed, recompiling",
                    input.display()
                );
            }
            CacheResult::Miss { entry_path }
        }
    }

    /// 编译成功后更新缓存条目.
    pub fn update(&self, cfg: &Config, input: &Path, entry_path: &Path) {
        let fingerprint = match self.compute_fingerprint(cfg, input) {
            Some(fp) => fp,
            None => return,
        };

        // 确保缓存目录存在
        if let Err(e) = fs::create_dir_all(&self.cache_dir) {
            if self.verbose {
                eprintln!(
                    "dcc: incremental: cannot create cache dir {}: {}",
                    self.cache_dir.display(),
                    e
                );
            }
            return;
        }

        // 写缓存条目
        if let Err(e) = fs::write(entry_path, &fingerprint) {
            if self.verbose {
                eprintln!(
                    "dcc: incremental: cannot write cache entry {}: {}",
                    entry_path.display(),
                    e
                );
            }
        }
    }

    /// 计算 (源内容 + 编译标志 + 编译器自身) 的指纹.
    ///
    /// 指纹 = hash(source_content, relevant_flags, exe_size, exe_mtime).
    /// 相关标志: 影响输出二进制的所有编译选项; 最后两项把「哪一版 dcc 写的条目」也钉进
    /// key（理由见下面第 6 条）。
    fn compute_fingerprint(&self, cfg: &Config, input: &Path) -> Option<String> {
        // 读源文件内容
        let source_content = fs::read(input).ok()?;

        let mut hasher = DefaultHasher::new();

        // 1. 源文件内容
        source_content.hash(&mut hasher);

        // 2. 影响输出的编译标志
        // 优化级别
        cfg.opt.hash(&mut hasher);
        // 标准
        (cfg.std.as_str()).hash(&mut hasher);
        // 目标三元组
        cfg.target.as_str().hash(&mut hasher);
        // 调试级别
        cfg.debug_level.hash(&mut hasher);
        // DWARF 版本
        cfg.dwarf_version.hash(&mut hasher);
        // 重定位 (.o vs .s)
        cfg.relocatable.hash(&mut hasher);
        cfg.emit_asm.hash(&mut hasher);
        // 内核代码模型
        cfg.kernel_code_model.hash(&mut hasher);
        // SSE
        cfg.sse.hash(&mut hasher);
        // Red zone
        cfg.red_zone.hash(&mut hasher);
        // Frame pointer
        cfg.frame_pointer.hash(&mut hasher);
        // LTO
        cfg.lto_enabled.hash(&mut hasher);
        // Function sections
        cfg.function_sections.hash(&mut hasher);
        // Sanitizers (hash each flag field)
        cfg.sanitizers.kasan.hash(&mut hasher);
        cfg.sanitizers.kernel_mode.hash(&mut hasher);
        cfg.sanitizers.hwasan.hash(&mut hasher);
        cfg.sanitizers.ubsan.hash(&mut hasher);
        cfg.sanitizers.kcsan.hash(&mut hasher);
        cfg.sanitizers.kcov.hash(&mut hasher);
        cfg.sanitizers.msan.hash(&mut hasher);
        cfg.sanitizers.dfsan.hash(&mut hasher);
        cfg.sanitizers.cfi_icall.hash(&mut hasher);
        cfg.sanitizers.cfi_vcall.hash(&mut hasher);
        cfg.sanitizers.cfi_nvcall.hash(&mut hasher);
        cfg.sanitizers.cfi_derived_cast.hash(&mut hasher);
        cfg.sanitizers.cfi_unrelated_cast.hash(&mut hasher);
        cfg.sanitizers.cfi_mfcall.hash(&mut hasher);
        cfg.sanitizers.fuzzer.hash(&mut hasher);
        cfg.sanitizers.cov_trace_cmp.hash(&mut hasher);
        cfg.sanitizers.cov_trace_switch.hash(&mut hasher);
        cfg.sanitizers.cov_8bit_counters.hash(&mut hasher);
        cfg.sanitizers.cov_pc_table.hash(&mut hasher);
        // Stack protector
        cfg.stack_protector.hash(&mut hasher);
        // Unwind tables
        cfg.unwind_tables.hash(&mut hasher);
        // Bounds check
        cfg.bounds_check.hash(&mut hasher);
        // Trapv
        cfg.trapv.hash(&mut hasher);
        // Auto var init
        cfg.auto_var_init_zero.hash(&mut hasher);
        // Strict aliasing
        cfg.strict_aliasing.hash(&mut hasher);
        // Strict overflow
        cfg.strict_overflow.hash(&mut hasher);
        // PIC: 改 switch 跳表能不能上表（直接改产物字节）
        cfg.pic.hash(&mut hasher);
        // Short wchar
        cfg.short_wchar.hash(&mut hasher);
        // No common
        cfg.no_common.hash(&mut hasher);
        // 对齐类标志: 直接改每条函数入口/循环头/跳转目标前的 nop 填充，
        // 不进指纹就会把「改了 -falign-* 之前」的代码当缓存命中端出去。
        cfg.align_loops.hash(&mut hasher);
        cfg.align_jumps.hash(&mut hasher);
        cfg.align_functions.hash(&mut hasher);
        // 同一类的缺口（本轮逐个对过 `cli::Config` 的字段表）：这几项都直接改字节，
        // 不进指纹就是拿旧字节当命中。
        //   * `patchable_function_entry`：函数入口前的 nop 前缀长度（内核传 =16,16）；
        //   * `cf_protection`：`branch/full` 时每个入口多一条 `endbr64`；
        //   * `asm_fallback`：`gas` 档整 TU 走文本+`as`，产物由另一条发射路径出；
        //   * `march`/`mcpu`：指令选型；
        //   * `data_sections`：每个变量独立 section，节表与重定位全变；
        //   * `file_prefix_maps`：调试目录前缀写进 DWARF。
        cfg.patchable_function_entry.hash(&mut hasher);
        cfg.cf_protection.hash(&mut hasher);
        (cfg.asm_fallback as u8).hash(&mut hasher);
        cfg.march.hash(&mut hasher);
        cfg.mcpu.hash(&mut hasher);
        cfg.data_sections.hash(&mut hasher);
        cfg.file_prefix_maps.hash(&mut hasher);
        // Ftrace mode
        (cfg.ftrace_mode as u8).hash(&mut hasher);
        // Stack clash protection
        cfg.stack_clash_protection.hash(&mut hasher);
        // Function return thunk extern
        cfg.function_return_thunk_extern.hash(&mut hasher);
        // Indirect branch thunk extern
        cfg.indirect_branch_thunk_extern.hash(&mut hasher);
        // CPU feature overrides (affect codegen)
        cfg.cpu_feature_overrides.avx.hash(&mut hasher);
        cfg.cpu_feature_overrides.avx2.hash(&mut hasher);
        cfg.cpu_feature_overrides.avx512f.hash(&mut hasher);
        cfg.cpu_feature_overrides.avx512bw.hash(&mut hasher);
        cfg.cpu_feature_overrides.avx512vl.hash(&mut hasher);
        cfg.cpu_feature_overrides.avx512dq.hash(&mut hasher);
        cfg.cpu_feature_overrides.fma.hash(&mut hasher);
        cfg.cpu_feature_overrides.bmi1.hash(&mut hasher);
        cfg.cpu_feature_overrides.bmi2.hash(&mut hasher);
        cfg.cpu_feature_overrides.cet_ss.hash(&mut hasher);
        cfg.cpu_feature_overrides.cet_ibt.hash(&mut hasher);

        // 3. 预处理器定义 (-D)
        for (name, value) in &cfg.defines {
            name.hash(&mut hasher);
            value.hash(&mut hasher);
        }

        // 4. 强制包含 (-include)
        for inc in &cfg.force_includes {
            inc.hash(&mut hasher);
        }

        // 5. 包含路径 (-I) - 影响头文件搜索
        for path in &cfg.include_paths {
            path.hash(&mut hasher);
        }

        // 6. 编译器自身的指纹：换二进制就必须作废缓存。
        crate::incremental::hash_compiler_binary(&mut hasher);

        let hash_value = hasher.finish();
        Some(format!("{:016x}", hash_value))
    }

    /// 计算缓存条目路径: `.dcc-cache/<source_path_hash>`
    fn entry_path(&self, input: &Path) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        // 用源文件的绝对路径 (或相对路径) 作为 key
        input.hash(&mut hasher);
        let path_hash = hasher.finish();
        self.cache_dir.join(format!("{:016x}.cache", path_hash))
    }
}

/// 判断是否应启用增量编译.
///
/// 条件:
/// - cfg.incremental == true (用户显式启用 -fcached)
/// - 非 -E / -M / -MM / -fsyntax-only 模式 (这些不产出目标文件)
/// - 非 stdin 输入 (input != "-")
/// - 非 -o - (管道输出)
pub fn should_use_incremental(cfg: &Config, input: &Path) -> bool {
    if !cfg.incremental {
        return false;
    }
    // 不产出目标文件的模式
    if cfg.preprocess_only || cfg.dep_no_compile || cfg.syntax_only {
        return false;
    }
    // stdin 输入
    if input.as_os_str() == "-" {
        return false;
    }
    // 管道输出
    if cfg.output.to_str() == Some("-") {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_config(source_path: &Path, output_path: &Path) -> Config {
        Config {
            inputs: vec![source_path.to_path_buf()],
            output: output_path.to_path_buf(),
            output_explicit: true,
            opt: 0,
            size_opt: false,
            std: crate::cli::StdMode::Gnu23,
            target: crate::cli::TargetTriple::host(),
            freestanding: false,
            include_paths: vec![],
            system_paths: vec![],
            quote_paths: vec![],
            after_paths: vec![],
            defines: vec![],
            dump_ast: false,
            verbose: false,
            // `Config` 新增了链接期透传参数（见 `cli.rs`），测试夹具跟着补齐，
            // 否则 `cargo test` 连编译都过不去（本次只补字段，不改用例语义）。
            host_link_args: vec![],
            relocatable: true,
            link_mode: false,
            emit_asm: false,
            asm_fallback: crate::cli::AsmFallback::default(),
            // 本轮补：指纹新增的 `cf_protection` 必须在夹具里有个确定值（只补字段，不改用例语义）。
            // 其余五项（`patchable_function_entry`/`march`/`mcpu`/`data_sections`/
            // `file_prefix_maps`）夹具本来就有，不在这里重复写。
            cf_protection: false,
            preprocess_only: false,
            dump_macros: false,
            force_includes: vec![],
            pch_opts: crate::preproc::PchOptions::default(),
            syntax_only: false,
            emit_module_file: None,
            dep_file: None,
            dep_target: None,
            gen_dep: false,
            dep_mmd_only: false,
            dep_phony: false,
            dep_no_compile: false,
            dep_quote_target: false,
            red_zone: true,
            frame_pointer: false,
            sse: true,
            kernel_code_model: false,
            auto_var_init_zero: false,
            stack_protector: false,
            unwind_tables: true,
            function_return_thunk_extern: false,
            indirect_branch_thunk_extern: false,
            patchable_function_entry: None,
            debug_level: 0,
            dwarf_version: 4,
            lto_enabled: false,
            function_sections: false,
            data_sections: false,
            gc_sections: false,
            specs: None,
            output_format: crate::output_format::OutputFormat::default(),
            sanitizers: crate::sanitizer::SanitizerFlags::default(),
            input_lang: crate::cli::InputLang::C,
            as_options: vec![],
            m32: false,
            m16: false,
            parallel_codegen: false,
            jobs: None,
            permissive: false,
            raw_args: vec![],
            built_link: false,
            output_kind: crate::cli::LinkerOutputKind::Executable,
            soname: None,
            linker_entry: None,
            linker_lto: false,
            exclude_libs: vec![],
            build_id: crate::link::BuildIdStyle::None,
            no_undefined_version: false,
            link_opts: crate::link::link_opts::LinkOpts::default(),
            link_libs: vec![],
            whole_archive_libs: vec![],
            lib_search_paths: vec![],
            gc_sections_root: vec![],
            no_semantic_interposition: false,
            semantic_interposition: false,
            fuse_ld: None,
            gdb_index: false,
            compress_debug: None,
            lto_partitions: None,
            trapv: false,
            bounds_check: false,
            warn_int_conversion: false,
            warn_int_truncation: false,
            warn_implicit_decl: true,
            warn_undef: false,
            warn_implicit_fallthrough: true,
            diag_color: crate::diagnostics::ColorPolicy::Auto,
            diag_format: crate::diagnostics::FormatStyle::Github,
            diag_show_template_tree: false,
            instrument_functions: false,
            bootstrap_compare: None,
            sysroot: None,
            vdso: None,
            linker_script: None,
            ftrace_mode: crate::cli::FtraceMode::None,
            stack_protector_guard_reg: crate::cli::StackGuardReg::Fs,
            stack_clash_protection: false,
            strict_aliasing: true,
            strict_overflow: true,
            pic: false,
            short_wchar: false,
            no_common: false,
            cli_warnings: vec![],
            incremental: true,
            cpu_feature_overrides: crate::cpuid::CpuFeatureOverrides::default(),
            file_prefix_maps: vec![],
            macro_prefix_maps: vec![],
            macro_backtrace_limit: 128,
            trigraphs: false,
            march: None,
            mtune: None,
            mcpu: None,
            arm_instruction_mode: crate::cli::ArmInstructionMode::default(),
            float_abi: crate::cli::FloatAbi::default(),
            fast_math: false,
            fp_contract_mode: crate::cli::FpContractMode::Default,
            align_loops: 0,
            align_jumps: 0,
            align_functions: 0,
            profile_parse: false,
            ipa_sra: crate::ipa_sra::IpaSraConfig::default(),
            profile: crate::profile::ProfileConfig::default(),
            cxx_native_mode: crate::cpp::feature_matrix::CxxNativeMode::Auto,
        }
    }

    #[test]
    fn cache_miss_when_output_missing() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let src = dir.join(format!("dcc-incr-test-{pid}.c"));
        let out = dir.join(format!("dcc-incr-test-{pid}.o"));

        fs::write(&src, "int main(void) { return 0; }\n").unwrap();
        let _ = fs::remove_file(&out);

        let cache_dir = dir.join(format!("dcc-incr-test-cache-{pid}"));
        let _ = fs::remove_dir_all(&cache_dir);
        let cache = IncrementalCache::with_cache_dir(cache_dir.clone(), false);
        let cfg = make_test_config(&src, &out);

        match cache.check(&cfg, &src) {
            CacheResult::Hit => panic!("expected miss when output missing"),
            CacheResult::Miss { .. } => {}, // expected
        }

        let _ = fs::remove_file(&src);
        let _ = fs::remove_file(&out);
        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn cache_hit_after_update() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let src = dir.join(format!("dcc-incr-hit-{pid}.c"));
        let out = dir.join(format!("dcc-incr-hit-{pid}.o"));

        fs::write(&src, "int foo(void) { return 42; }\n").unwrap();
        // 创建输出文件 (模拟编译完成)
        fs::write(&out, b"\x7fELF").unwrap();

        let cache_dir = dir.join(format!("dcc-incr-hit-cache-{pid}"));
        let _ = fs::remove_dir_all(&cache_dir);
        let cache = IncrementalCache::with_cache_dir(cache_dir.clone(), false);
        let cfg = make_test_config(&src, &out);

        // 首次: 无缓存条目 → miss
        let entry_path = match cache.check(&cfg, &src) {
            CacheResult::Hit => panic!("expected miss on first check"),
            CacheResult::Miss { entry_path } => entry_path,
        };

        // 更新缓存
        cache.update(&cfg, &src, &entry_path);

        // 再次检查: 应命中
        match cache.check(&cfg, &src) {
            CacheResult::Hit => {}, // expected
            CacheResult::Miss { .. } => panic!("expected hit after update"),
        }

        // 清理
        let _ = fs::remove_file(&src);
        let _ = fs::remove_file(&out);
        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn cache_invalidated_on_source_change() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let src = dir.join(format!("dcc-incr-chg-{pid}.c"));
        let out = dir.join(format!("dcc-incr-chg-{pid}.o"));

        fs::write(&src, "int v1(void) { return 1; }\n").unwrap();
        fs::write(&out, b"\x7fELF").unwrap();

        let cache_dir = dir.join(format!("dcc-incr-chg-cache-{pid}"));
        let _ = fs::remove_dir_all(&cache_dir);
        let cache = IncrementalCache::with_cache_dir(cache_dir.clone(), false);
        let cfg = make_test_config(&src, &out);

        // 首次 miss + update
        let entry_path = match cache.check(&cfg, &src) {
            CacheResult::Miss { entry_path } => entry_path,
            CacheResult::Hit => panic!("expected miss"),
        };
        cache.update(&cfg, &src, &entry_path);

        // 修改源文件
        fs::write(&src, "int v2(void) { return 2; }\n").unwrap();

        // 应 miss (源变了)
        match cache.check(&cfg, &src) {
            CacheResult::Hit => panic!("expected miss after source change"),
            CacheResult::Miss { .. } => {}, // expected
        }

        let _ = fs::remove_file(&src);
        let _ = fs::remove_file(&out);
        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn cache_invalidated_on_flag_change() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let src = dir.join(format!("dcc-incr-flag-{pid}.c"));
        let out = dir.join(format!("dcc-incr-flag-{pid}.o"));

        fs::write(&src, "int x(void) { return 0; }\n").unwrap();
        fs::write(&out, b"\x7fELF").unwrap();

        let cache_dir = dir.join(format!("dcc-incr-flag-cache-{pid}"));
        let _ = fs::remove_dir_all(&cache_dir);
        let cache = IncrementalCache::with_cache_dir(cache_dir.clone(), false);
        let mut cfg = make_test_config(&src, &out);

        // 首次: -O0
        let entry_path = match cache.check(&cfg, &src) {
            CacheResult::Miss { entry_path } => entry_path,
            CacheResult::Hit => panic!("expected miss"),
        };
        cache.update(&cfg, &src, &entry_path);

        // 改为 -O2
        cfg.opt = 2;

        // 应 miss (标志变了)
        match cache.check(&cfg, &src) {
            CacheResult::Hit => panic!("expected miss after flag change"),
            CacheResult::Miss { .. } => {}, // expected
        }

        let _ = fs::remove_file(&src);
        let _ = fs::remove_file(&out);
        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn should_use_incremental_respects_mode() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let src = dir.join(format!("dcc-incr-mode-{pid}.c"));
        let out = dir.join(format!("dcc-incr-mode-{pid}.o"));
        fs::write(&src, "int x;\n").unwrap();
        fs::write(&out, b"").unwrap();

        let mut cfg = make_test_config(&src, &out);
        cfg.incremental = true;

        // 正常 -c 模式: 应启用
        assert!(should_use_incremental(&cfg, &src));

        // -E 模式: 不启用
        cfg.preprocess_only = true;
        assert!(!should_use_incremental(&cfg, &src));
        cfg.preprocess_only = false;

        // stdin: 不启用
        let stdin = Path::new("-");
        assert!(!should_use_incremental(&cfg, stdin));

        // -fcached 未启用: 不启用
        cfg.incremental = false;
        assert!(!should_use_incremental(&cfg, &src));

        let _ = fs::remove_file(&src);
        let _ = fs::remove_file(&out);
    }
}

/// 把「当前这个编译器二进制」的身份混进任意 hasher。**只留一份实现**：compile 缓存
/// （本模块 `compute_fingerprint`）与 include 预处理缓存（`preproc.rs::hash_macro_state`）
/// 都必须带这一层。
///
/// 为什么必须：指纹原本只覆盖「源内容 + 影响输出的选项」，于是改一行前端/codegen 重编
/// dcc 之后，全树仍会命中上一版编译器写下的条目。两个实测现场：
///   * 修好解析器缺陷、`make clean` 重跑，产物里还是旧编译器的字节（几千条旧条目）；
///   * 修好预处理器「宏体内跨行注释」的缺陷后，真树仍报同一个错 —— 因为 `~/.dcc` 里 12 GB
///     的旧预处理输出在回放。当时只能手删缓存目录；而「记得手动清缓存」是纪律条款不是机制，
///     目标要求「每一个字节都由当前这版 dcc 产生」，所以必须自动化。
///
/// 取**内容**而不是 size+mtime。两条都有实测代价：
///   * 想要的判据是「字节相同的编译器 ⇒ 产物字节相同 ⇒ 缓存可继续用」。mtime 给不出这个
///     判据，而且反向也不成立：按 §5.0e 用 `install -m 0755` 把 `target/release/dcc` 装成
///     全树只读快照时，两个文件 md5 逐字相同、mtime 不同一 ⇒ 12 GB 预处理缓存整批作废。
///     量到的代价：一个内核 TU 冷读 46.2 s、同一份输出命中缓存 0.8 s；~1 万个对象就是
///     几十小时纯重复劳动（本机实测：`/tmp/twice.sh`）。
///   * 读一次 12 MB 是毫秒级（`[u8]` 的 `Hash` 走 `write` 批量路径，不是逐字节），相对
///     它省下的一次全树重预处理可以忽略。
/// 拿不到就退回 size + mtime（只少一层精度，与旧行为一致），再拿不到就不混这一项。
pub fn hash_compiler_binary<H: std::hash::Hasher>(hasher: &mut H) {
    let Ok(exe) = std::env::current_exe() else { return };
    if let Ok(bytes) = fs::read(&exe) {
        bytes.hash(hasher);
        return;
    }
    if let Ok(md) = fs::metadata(&exe) {
        md.len().hash(hasher);
        if let Ok(t) = md.modified() {
            match t.duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => {
                    d.as_secs().hash(hasher);
                    d.subsec_nanos().hash(hasher);
                },
                Err(_) => {
                    // 系统时钟早于 epoch：拿不到单调可用的读数，这一层保护本轮不生效
                    0u64.hash(hasher);
                },
            }
        }
    }
}
