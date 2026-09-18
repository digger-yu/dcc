# C++ 测试函数名统一方案：里程碑前缀 → `cpp_*`

> 目标：把以里程碑编号命名的 C++ 相关测试/单测，统一改成以 `cpp` 开头的功能语义命名。  
> 原则：**见名知意、简单、高效**——默认只替换前缀；后缀必须与**真实断言**一致，不得名不副实。  
> 范围：`tests/integration.rs`、`src/cpp/lower.rs`、相关文档、label/临时路径/assert 文案中的旧标识。  
> 非目标：不改 C 侧里程碑测试；fixture 文件路径一般不动。  
> 文档版本：**v3**（修正 v2 中错误的 `_runtime` 标注、遗漏的 `p2_*`、统计与规则矛盾）

---

## 1. 现状分析

### 1.1 命名从哪来

`docs/roadmap.md` 用里程碑推进；早期测试直接用 `mNN_` 作前缀（`m17_auto_deduce`、`m18_chrono`、`m26_coroutine_state_machine`…）。仓库里同时存在功能命名（`cpp98_*`、`cpp20_*`、`coroutine_co_await_runtime`）和阶段号命名（`p2_1_*`…`p2_6_*`），两套以上并存。

### 1.2 混语言问题（改名核心动机）

- `m26_*`：C++（coroutine/modules…）与 C 预处理（elifdef/embed…）共用前缀。  
- `m27_*`：C++（typeid/template/modules…）与 C 属性/builtin 共用前缀。  
- `cargo test m26_` / `m27_` 无法按语言过滤。

### 1.3 规模（按源码重新点数，v3 更正）

| 类别 | 实际数量 | 本次是否改 |
|------|----------|------------|
| `fn m17_*` | **17** | 是（全 C++） |
| `fn m18_*` | **14**（v2 误写 16） | 是（全 C++） |
| `fn m26_*` 总计 | **21** | 仅 C++ 子集 **13** + 边界 1；C 预处理 7 不改 |
| `fn m27_*` 总计 | **26** | 仅 C++ 子集 **5**；C 属性等 21 不改 |
| `fn p2_*` | **6**（v2 遗漏） | 是（全 C++） |
| `src/cpp/lower.rs` `m10_*` + `m26_concept_*` | **13+1** | 是 |
| 无前缀/混排 C++（如 `coroutine_co_await_runtime`） | 若干 | 对齐 |

CI 跑全量 `cargo test`，不按 `m17_` 过滤 → 改名不破坏门禁。

### 1.4 应保留的优点

- `tests/fixtures/cpp/*.cpp` 已是功能名，路径不动。  
- 已有 `cpp98_*` / `cpp20_*` 可作参照。

---

## 2. 目标命名规范（简化 + 断言对齐）

### 2.1 默认

```text
cpp_<feature>
```

去掉 `mNN_` / `m10_` / `p2_N_` 前缀；不强加 domain。

### 2.2 可选后缀（v3：按函数体断言打标，禁止凭名字猜）

| 后缀 | 判定标准（满足其一） |
|------|----------------------|
| （无后缀） | 仅 `status.success()` 编译/产出 `.o`/`.s` |
| `_runtime` | 编译成功后 **host gcc 链接并执行 exe**，且断言 `run.status` / `rc == 0` |
| `_rejected` | 断言 `!status.success()` 或必须出现诊断（负向） |
| `_smoke` | 冒烟（可与 feature 连写，如 `cpp_sfinae_smoke`） |
| `_links` | 多 TU 链接 / COMDAT / 弱符号类（可选，也可无后缀靠 body） |

**配对后缀（允许 `_ok`）**：仅当与 `_rejected` 成对出现时允许 `_ok`  
（`cpp_template_depth_ok` ↔ `cpp_template_depth_limit_rejected`；  
`cpp_odr_identical_imports_ok` ↔ `cpp_odr_conflict_rejected`）。  
禁止再发明 `_works` / `_good` / `_fine`。

**不得**给「只编译」的测试挂 `_runtime`（见 §3.2 修正表）。  
旧名里的 `_runtime` 若函数体并不跑，**改名时去掉**，以函数体为准。

### 2.3 标准版本前缀

已有 `cpp98_*` / `cpp11_*` / `cpp20_*` / `cpp23_*` **保持不动**（其 `_compile` 后缀可保留，避免无谓 diff）。

### 2.4 长度与可过滤

- 软目标：≤ ~30 字符、≤ 4 段 snake；优先去后缀/domain，不缩写实义词。  
- 全局过滤：`cargo test cpp_`；子串如 `cargo test chrono`。  
- 注意：`cargo test cpp_typeid` 会同时命中 `cpp_typeid*` 与 `cpp_typeid_dynamic_rtti`——预期行为，可接受。

### 2.5 同步改名的标识（v3 扩充）

不止 `fn` 名，还包括：

1. **`run_*("label")` 的 kebab label**（`m17-decltype` → `cpp-decltype`）  
2. **`format!("dcc-m17-weak-…")` 等临时路径字符串**（仅 C++ 相关）  
3. **`assert!(…, "… old_name …")` 文案中的旧函数名**（如 `"p2_2_typeid compile failed"`、`"m18_template_depth_ok compile failed"`）  
4. **doc comment**：可保留 `M17.0:` 历史叙述；**写死旧 `fn` 名的句子必须改**

C 侧 label（`m26-embed`、`m27-attr-alias`…）与 C 临时路径 **本次不改**。

---

## 3. 完整改名映射

> 下表「断言」来自读函数体：`compile` = 只编译；`runtime` = 链接+执行；`neg` = 期望失败；`link` = 多文件链接。

### 3.1 `m17_*`（17）

| 旧名 | 新名 | 断言 | label |
|------|------|------|-------|
| `m17_smoke` | `cpp_smoke` | compile | `cpp-smoke` |
| `m17_auto_deduce` | `cpp_auto_deduce` | compile | `cpp-auto-deduce` |
| `m17_decltype` | `cpp_decltype` | compile | `cpp-decltype` |
| `m17_decltype_auto` | `cpp_decltype_auto` | compile | `cpp-decltype-auto` |
| `m17_explicit_ctor` | `cpp_explicit_ctor` | compile | `cpp-explicit-ctor` |
| `m17_final_override` | `cpp_final_override` | compile | `cpp-final-override` |
| `m17_friend` | `cpp_friend` | compile | `cpp-friend` |
| `m17_if_constexpr` | `cpp_if_constexpr` | compile | `cpp-if-constexpr` |
| `m17_inheriting_ctor` | `cpp_inheriting_ctor` | compile | `cpp-inheriting-ctor` |
| `m17_mutable` | `cpp_mutable` | compile | `cpp-mutable` |
| `m17_new_array` | `cpp_new_array` | compile | `cpp-new-array` |
| `m17_noexcept` | `cpp_noexcept` | compile | `cpp-noexcept` |
| `m17_placement_new` | `cpp_placement_new` | compile | `cpp-placement-new` |
| `m17_typeid` | `cpp_typeid` | compile | `cpp-typeid` |
| `m17_volatile_member` | `cpp_volatile_member` | compile | `cpp-volatile-member` |
| `m17_member_ptr` | `cpp_member_ptr` | compile | `cpp-member-ptr` |
| `m17_weak_vtable_multi_file_link` | `cpp_weak_vtable_multifile_link` | link+run | 见下「临时路径」 |

同步临时路径：`dcc-m17-weak-tu1/2`、`dcc-m17-weak-*` → `dcc-cpp-weak-tu1/2`、`dcc-cpp-weak-*`。

### 3.2 `m18_*`（14）— **v3 重大修正：全部为 compile，禁止 `_runtime`**

全部走 `run_cpp_object_test` + `assert!(out.status.success())`，**没有** host 链接执行。

| 旧名 | 新名 | 断言 | 说明 |
|------|------|------|------|
| `m18_chrono` | `cpp_chrono` | compile | ~~`cpp_chrono_runtime`~~（v2 错误） |
| `m18_thread_mutex` | `cpp_thread_mutex` | compile | 同上 |
| `m18_condition_variable` | `cpp_condition_variable` | compile | 同上 |
| `m18_filesystem` | `cpp_filesystem` | compile | 同上 |
| `m18_expected` | `cpp_expected` | compile | 同上 |
| `m18_flat_map` | `cpp_flat_map` | compile | 同上 |
| `m18_multi_index` | `cpp_multi_index` | compile | 同上 |
| `m18_regex` | `cpp_regex` | compile | 同上 |
| `m18_ranges` | `cpp_ranges` | compile | 同上 |
| `m18_concept` | `cpp_concept` | compile | 同上 |
| `m18_modules` | `cpp_modules` | compile | |
| `m18_template_depth_ok` | `cpp_template_depth_ok` | compile | 与 `_rejected` 成对，保留 `_ok` |
| `m18_template_depth_limit_enforced` | `cpp_template_depth_limit_rejected` | **neg** | 带 `#[ignore]`，改名后仍保留 ignore |
| `m18_sfinae_smoke` | `cpp_sfinae_smoke` | compile | |

label：`m18-chrono` → `cpp-chrono`，`m18-cv` → `cpp-condition-variable`，`m18-sfinae` → `cpp-sfinae` 等。

### 3.3 `m26_*` 的 C++ 子集

| 旧名 | 新名 | 断言 | 说明 |
|------|------|------|------|
| `m26_coroutine_state_machine` | `cpp_coroutine_state_machine` | compile | 无 `_runtime` |
| `m26_coroutine_runtime` | `cpp_coroutine_runtime` | **runtime** | 保留 |
| `m26_modules_bmf` | `cpp_modules_bmf` | compile | ~~`cpp_modules_bmf_runtime`~~（v2 错误） |
| `m26_module_cross_tu_import` | `cpp_module_cross_tu_import` | **runtime** | 末尾链接执行；可加 `_runtime`：`cpp_module_cross_tu_import_runtime`（可选，二选一需全表一致：**建议加上**） |
| `m26_module_partition_import` | `cpp_module_partition_import_runtime` | **runtime** | 建议加 `_runtime` |
| `m26_odr_conflict_rejected` | `cpp_odr_conflict_rejected` | **neg** | |
| `m26_odr_identical_imports_ok` | `cpp_odr_identical_imports_ok` | compile | 成对 `_ok` |
| `m26_enum_semantics_runtime` | `cpp_enum_semantics_runtime` | **runtime** | 保留 |
| `m26_member_parser_runtime` | `cpp_member_parser_runtime` | **runtime** | 保留 |
| `m26_auto_deduction_runtime` | `cpp_auto_deduction_runtime` | **runtime** | 保留 |
| `m26_noexcept_expr_runtime` | `cpp_noexcept_expr_runtime` | **runtime** | 保留 |
| `m26_virtual_inheritance_runtime` | `cpp_virtual_inheritance_runtime` | **runtime** | 保留 |
| `m26_gcc_compat_runtime` | `cpp_gcc_compat` | **compile** | 旧名 `_runtime` 名不副实，**去掉** |

**边界（建议一期不改）**：`m26_char16_char32_compiles_and_runs`  
fixture 为 `fixtures/c/char16_char32.c`（C11/C++11 共用类型），断言为 C 路径 `run_compile_and_run`。  
→ 留在 C 侧里程碑或二期与 C 命名一起处理，**不要**强行 `cpp_char16_*`。

**不改的 m26（C 预处理，7）**：`elifdef_elifndef` / `has_macros` / `embed_*` / `has_embed` / `pragma_once` / `include_next`。

### 3.4 `m27_*` 的 C++ 子集（5）

全部 compile-only：

| 旧名 | 新名 | label |
|------|------|-------|
| `m27_mutable_const_method` | `cpp_mutable_const_method` | `cpp-mutable-const` |
| `m27_member_ptr_deref_typed` | `cpp_member_ptr_deref` | `cpp-member-ptr-deref` |
| `m27_typeid_dynamic_rtti` | `cpp_typeid_dynamic_rtti` | `cpp-typeid-dynamic` |
| `m27_template_specialization` | `cpp_template_specialization` | `cpp-template-spec` |
| `m27_modules_odr` | `cpp_modules_odr` | `cpp-modules-odr` |

**不改的 m27（C，21）**：`attr_*` / `builtin_*` / `fortify` / `lowlevel` / `static_chain` 及对应 `m27-*.c` fixture。

### 3.5 `p2_*`（6）— v2 遗漏，全部纳入

| 旧名 | 新名 | 断言 | assert 文案同步 |
|------|------|------|-----------------|
| `p2_1_member_ptr_dotstar` | `cpp_member_ptr_dotstar` | compile | `"p2_1_member_ptr compile failed"` |
| `p2_2_typeid_rtti` | `cpp_typeid_rtti` | compile | `"p2_2_typeid compile failed"` |
| `p2_3_template_arg_syntax` | `cpp_template_arg_syntax` | compile | `"p2_3_template_arg compile failed"` |
| `p2_4_new_arr_mangler` | `cpp_new_arr_mangler` | compile | `"p2_4_new_arr_mangler compile failed"` |
| `p2_5_new_array_alloc` | `cpp_new_array_alloc` | compile | `"p2_5_new_array_alloc compile failed"` |
| `p2_6_placement_new` | `cpp_placement_new_ctor` | compile | `"p2_6_placement_new compile failed"`；与 `cpp_placement_new` 用 fixture 区分 |

**注意近重复（不合并，仅澄清命名）**：

| 关系 | fixture | 区分 |
|------|---------|------|
| `cpp_typeid`（原 m17） | `typeid.cpp` | 基础 typeid |
| `cpp_typeid_rtti`（原 p2_2） | `typeid_rtti.cpp` | 静态/动态 typeinfo |
| `cpp_typeid_dynamic_rtti`（原 m27） | 同 `typeid_rtti.cpp` | 与 p2_2 **同 fixture**，断言可能重叠 → 改名后 PR 里注明「暂保留，后续可合并」 |

### 3.6 其它存量 C++ 测试（对齐）

| 旧名 | 新名 | 备注 |
|------|------|------|
| `coroutine_co_await_runtime` | `cpp_coroutine_co_await_runtime` | 真 runtime；与 `cpp_coroutine_runtime` 对齐，勿缩成 `cpp_co_await_*` |
| `cpp_class_with_ctor_lowers_and_compiles` | `cpp_class_with_ctor` | 函数体为空 stub，可改可不改（低优先级） |
| `cpp_template_vector_int_instantiation` | `cpp_vector_int_instantiation` | **compile/asm 断言**，勿加 `_runtime` |
| `cpp_vector_header_is_template_class` | 保持 | 已清晰 |
| `comdat_group_cpp_template_instantiation` | `cpp_comdat_template_instantiation` | 前缀统一 |
| `cpp_feature_keywords_all_detected` | 保持 **或** `cpp_feature_keywords_detected` | **v3：二选一写死，禁止既改又不改**（v2 矛盾） |
| `cpp_strip_keywords_all_removed` | 保持 **或** `cpp_strip_keywords_removed` | 低优先级，同上二选一 |
| `cpp_namespace_typedef_qualify_name_consistent` | 保持 **或** `cpp_namespace_typedef_qualify` | 低优先级 |

**推荐一期策略**：只强制  
`coroutine_co_await_*`、`comdat_group_cpp_*`、`p2_*`、`m10/m17/m18/m26C++/m27C++`；  
已带 `cpp` 前缀的长名**可全部保持不动**，减少无谓 churn。

**保持不动**：`cpp98_*` `cpp11_*` `cpp20_features_compile` `cpp23_features_compile`。

### 3.7 `src/cpp/lower.rs` 单测

| 旧名 | 新名 |
|------|------|
| `m10_sfinae_substitute_basic` | `cpp_sfinae_substitute` |
| `m10_partial_spec_pointer_matches` | `cpp_partial_spec_pointer_matches` |
| `m10_concept_size_le` | `cpp_concept_size_le` |
| `m10_concept_same_as` | `cpp_concept_same_as` |
| `m26_concept_sfinae_integration` | `cpp_concept_sfinae_integration` |
| `m10_cxa_guard_state_machine` | `cpp_cxa_guard_state_machine` |
| `m10_init_list_emit_c` | `cpp_init_list_emit_c` |
| `m10_module_partition_signature` | `cpp_module_partition_signature` |
| `m10_coroutine_await_yield_state` | `cpp_coroutine_await_yield_state` |
| `m10_arrow_chain_resolve` | `cpp_arrow_chain_resolve` |
| `m10_dynamic_cast_cross_virtual` | `cpp_dynamic_cast_cross_virtual` |
| `m10_ctor_delegation` | `cpp_ctor_delegation` |
| `m10_member_ptr_emit` | `cpp_member_ptr_emit` |
| `lower_m10_static_local_emit_cxa_guard` | `cpp_static_local_cxa_guard` |

注释 `// ===== M10+ tests =====` → `// ===== C++ lower unit tests =====`。

### 3.8 文档同步

| 文件 | 旧引用 → 新名 |
|------|----------------|
| `docs/roadmap.md` | `m17_inheriting_ctor`、`m17_friend`、`m18_*` 列表（11 个 → 实际 14 个测试名全换） |
| `docs/support_features.md` | `m26_coroutine_runtime`、`m26_modules_bmf`、`m26_module_*`、`coroutine_co_await_runtime` |
| `docs/cpp_native_backend_implementation_plan.md` | `m26_virtual_inheritance_runtime` |
| `docs/architecture.md` | 若有 C++ 旧名；`m16_*` 可不动 |
| `docs/ci_guide.md`（可选） | 增加 `cargo test cpp_` |

约定句（写入 roadmap 或 architecture）：

> C++ 测试/单测统一 `cpp_*` 前缀，规范见 `docs/cpp_test_naming_unification_plan.md`。里程碑号只用于路线图叙述，不作测试标识符。

---

## 4. 一期明确不改

| 项 | 原因 |
|----|------|
| `m1_*` `m2_*` `m3_*` `m5_*` | C / 自举 |
| `m15_*`（含 constexpr） | C 路径 |
| `m16_*` | 优化器 |
| `m19_*` / `m23_*` | 预处理 / C 属性 |
| `m26_*` C 预处理 7 项 | 与 C++ 无关 |
| `m26_char16_char32_*` | C fixture / C11 路径（边界，见 3.3） |
| `m27_*` C 子集 21 项 + `tests/fixtures/c/m27_*.c` | C 侧二期 |
| C 侧 `m26-*` / `m27-*` label 与 `dcc-m27-*` 路径 | 随 C 测试 |
| CI workflows | 全量 test |

---

## 5. 风险与注意（v3 强化）

1. **后缀与断言不一致是最高优先级风险**：v2 给 m18 全加了 `_runtime` 是错的；执行时以本表「断言」列为准，或以函数体 `run_exe` / `Command::new("gcc")` + 执行为准。  
2. **`#[ignore]`**：`m18_template_depth_limit_enforced` 仍带 ignore，改名勿删。  
3. **同步面**：`fn` + label + `format!` 临时路径 + assert 文案 + 文档；只改 `fn` 会留下 `p2_2_typeid compile failed` 这类残留。  
4. **近重复**：`p2_2_typeid_rtti` 与 `m27_typeid_dynamic_rtti` 同 fixture——改名后评估是否合并（非本次必做）。  
5. **冲突扫描**：改前 `rg "fn cpp_" tests src/cpp/lower.rs`（当前无与新表碰撞）。  
6. **过滤习惯**：`cargo test m17` → `cargo test cpp_`。  
7. **独立 rename 提交**，message 写明 no behavior change。  
8. **`tests/errors.rs`**：预期零改动。

---

## 6. 实施步骤

### A. 准备

```bash
rg -n "fn cpp_" tests/integration.rs src/cpp/lower.rs
rg -n "fn (m17_|m18_|m10_|p2_)" tests/integration.rs src/cpp/lower.rs
rg -n "fn m26_|fn m27_" tests/integration.rs
rg -n '"p2_|m17-|m18-|m26-coroutine|m26-modules|m26-virtual|m26-enum|m26-auto|m26-noexcept|m26-member|m26-gcc|m27-mutable|m27-member|m27-typeid|m27-tpl|m27-modules|dcc-m17-weak' tests/integration.rs
```

### B. 代码

1. `tests/integration.rs`：按 3.1–3.6 改 `fn`；同步 label、`dcc-m17-weak*`、assert 文案、写死旧名的注释。  
2. `src/cpp/lower.rs`：按 3.7。  
3. 存量对齐（3.6 推荐策略）可单独第二小提交。

### C. 文档

按 3.8。

### D. 验证

```bash
cargo build --tests
cargo test --test integration
cargo test --lib
cargo test cpp_ -- --list
cargo test m17_ -- --list    # 预期空
cargo test m18_ -- --list    # 预期空
cargo test m10_ -- --list    # 预期空
cargo test m26_ -- --list    # 仅 C 预处理 + char16 边界
cargo test m27_ -- --list    # 仅 C 属性/builtin
cargo test p2_ -- --list     # 预期空
```

验收清单：

- [ ] 17 + 14 + 13(C++ m26) + 5(m27 C++) + 6(p2) + lower 单测均以 `cpp` 开头  
- [ ] 所有带 `_runtime` 的新名在函数体内能观察到链接+执行  
- [ ] 所有 `m18_*` 新名**无** `_runtime`  
- [ ] `m26_gcc_compat_runtime` → `cpp_gcc_compat`（无 `_runtime`）  
- [ ] `m26_modules_bmf` → `cpp_modules_bmf`（无 `_runtime`）  
- [ ] assert 文案 / label / `dcc-m17-weak` 无残留旧标识  
- [ ] 文档无作为现行 ID 的旧 C++ 里程碑名  
- [ ] 全量测试与改名前同级通过（unix 用例需 Linux/WSL/CI）

### E. 提交信息建议

```text
refactor(test): rename C++ milestone tests to cpp_* prefix

Unify m17/m18/m26(C++)/m27(C++)/p2_*/m10 lower unit tests under
feature-based cpp_* names. Suffixes follow actual assertions
(compile vs runtime vs rejected), not historical milestone labels.
No behavior change; fixtures unchanged; labels/assert text/docs updated.
```

---

## 7. 二期预留（C 侧）

| 前缀 | 范围 |
|------|------|
| `c_lang_*` | m1/m2/m3 |
| `c_pp_*` | m19 / m26 C 预处理 / char16（若留 C） |
| `c_attr_*` | m23 / m27 C 属性 |
| `c_builtin_*` | m27 builtin / fortify |
| `opt_*` | m16 |
| `bootstrap_*` | m5 |
| `c_const_*` | m15 |

`tests/fixtures/c/m27_*.c` → `attr_alias.c` 等，与已有 `attr_retain.c` 对齐。

---

## 8. 执行默认策略（摘要）

1. 只改 C++；映射以本文第 3 节**断言列**为准。  
2. 默认 `cpp_<feature>`；`_runtime` / `_rejected` / 成对 `_ok` 必须与函数体一致。  
3. **删除 v2 对 m18 / modules_bmf / gcc_compat 的错误 `_runtime`**。  
4. 一期覆盖 **含 p2_***（v2 遗漏）。  
5. 同步 label、临时路径、assert 文案、文档；fixture 路径不动。  
6. 独立纯 rename 提交。

---

## 9. 附录：改名后 grep

```bash
rg -n "fn (m17_|m18_|m10_|p2_)" tests/integration.rs src/cpp/lower.rs
rg -n "fn m26_" tests/integration.rs   # 仅 C 预处理 (+ char16)
rg -n "fn m27_" tests/integration.rs   # 仅 C 属性/builtin
rg -c "fn cpp_" tests/integration.rs src/cpp/lower.rs
rg -n '"p2_|dcc-m17-weak|m17-|m18-chrono|m18-sfinae|m26-coroutine|m26-modules-bmf' tests/integration.rs
rg -n "m17_inheriting_ctor|m18_chrono|m26_coroutine_runtime|m27_typeid_dynamic_rtti" docs --glob '!cpp_test_naming*'
```

---

## 10. v3 相对 v2 的修正记录

| # | 问题 | 修正 |
|---|------|------|
| 1 | m18 全表误标 `_runtime`（实为 compile-only） | §3.2 改为无后缀 |
| 2 | `cpp_modules_bmf_runtime` 名不副实 | 改为 `cpp_modules_bmf` |
| 3 | `m26_gcc_compat_runtime` 体不执行 | 改为 `cpp_gcc_compat` |
| 4 | 遗漏 `p2_1`…`p2_6` | §3.5 全表纳入 |
| 5 | m18 数量写成 16 | 实际 14 |
| 6 | 3.5 对 `cpp_feature_keywords_*` 既改又不改 | 改为二选一，推荐保持 |
| 7 | `coroutine_co_await` 缩成 `cpp_co_await` 与 coroutine 系列不齐 | 改为 `cpp_coroutine_co_await_runtime` |
| 8 | char16 归入 C++ 边界不清 | 一期不改，归 C 边界 |
| 9 | 同步范围只提 label | 补 assert 文案、`dcc-m17-weak*` |
| 10 | 模板深度测试分类易被脚本误判 | 明确 `_ok` 正向、`_rejected` 负向且带 `#[ignore]` |

---

*文档版本：v3 — 按函数体断言校正后缀；补 p2；修正统计与矛盾。执行后可在第 3 节加「完成状态」列。*
