# 工作包 IFUNC：解决「IFUNC 使链接静默从动态降级为静态」

负责人：工作包 IFUNC。约束依据见任务书；既有论证见 `docs/ssa-pass-upgrade-plan.md`
§7.38 / §7.31 未修项 2 / §7.40「仍未修」第 6 项，本文**不重复论证**，只做实测定实、
方向落地与缺口边界。

---

## 一、现状实测（先定实两处矛盾的说话）

`src/driver.rs:2717` 注释称「codegen 已生成 resolver 和 STT_GNU_IFUNC 符号」；
§7.38 称「resolver 机器码是内置链接器现场拼的，`ld` 拿到的 IFUNC 符号在 `.o` 里没有
对应的 resolver 体」。二者矛盾，必须自己定实。

夹具 `tests/fixtures/c/attr_target_clones.c`（以及最小化变体）经
`dcc -O0 -c` 产出的 `.o` 实测：

**`readelf -sW`（符号类型，本地化输出里 `<操作系统专用>：10` = STT_GNU_IFUNC）**

```
 94: 0000000000000000 100 <操作系统专用>：10  GLOBAL DEFAULT 1 compute
 95: 0000000000000068  82 <操作系统专用>：10  GLOBAL DEFAULT 1 multiply
 96: 00000000000000c0 374 FUNC               GLOBAL DEFAULT 1 main
 97: 0000000000000000 100 FUNC               GLOBAL DEFAULT 1 compute.sse4.2
 98: 0000000000000000 100 FUNC               GLOBAL DEFAULT 1 compute.default
 99: 0000000000000068  82 FUNC               GLOBAL DEFAULT 1 multiply.avx
100: 0000000000000068  82 FUNC               GLOBAL DEFAULT 1 multiply.sse4.2
101: 0000000000000068  82 FUNC               GLOBAL DEFAULT 1 multiply.default
```

**`readelf -rW`（重定位）**

```
00000000000000f7  0000005e00000004 R_X86_64_PLT32  compute()  compute - 4
0000000000000132  0000005f00000004 R_X86_64_PLT32  multiply() multiply - 4
```

无 `.iplt` 节，无 `R_X86_64_IRELATIVE`（IRELATIVE 只在最终链接产生，可重定位 `.o` 里
本就不该有，但**也没有任何指向 resolver 的记录**）。

**`objdump -d -j .text`（看 IFUNC 符号 st_value 处到底是什么）**

```
0000000000000000 <compute.default>:
   0: 41 55                push   %r13
   2: 49 89 e5             mov    %rsp,%r13
   ...
  27: 48 c7 c1 02 00 00 00 mov    $0x2,%rcx       ; x*2
  3b: 48 c7 c1 01 00 00 00 mov    $0x1,%rcx       ; +1
```
`objdump -d | grep -i cpuid` → **空**（整个 `.o` 无 CPUID 指令）。

### 定实结论（决定一切后续判断）

1. `.o` **确实带 STT_GNU_IFUNC(type=10) 符号**（`compute`/`multiply`）——driver.rs
   注释里「STT_GNU_IFUNC 符号」这半句**为真**。
2. 但这两个 IFUNC 符号的 `st_value` = 0x0 / 0x68，与 `compute.sse4.2`、
   `compute.default`（`multiply.*`）**完全同址**，指向的就是实现体（`x*2+1`），
   **不是 resolver**。全 `.o` 无 CPUID、无 resolver 符号、无 IRELATIVE 指向体。
   → §7.38「`.o` 里没有对应 resolver 体」**为真**；driver.rs 注释「codegen 已生成
   resolver」**为假**（codegen 只发了 IFUNC 类型标记，没发生成 resolver）。

即：codegen 把 IFUNC「标了类型却没配 resolver 机器码」，resolver 由内置链接器
`src/link/target_clones.rs` 现场拼 —— 所以宿主 `ld` 拿到的是「有 IFUNC 类型、无 resolver
可执行」的残符号，只能改道内置链接器。

### 改道后的三种产物形态（实测）

| 输入 | 链接路径 | 产物（`file`/`readelf -d` 实测） | 退出码 |
|---|---|---|---|
| `printf`-only | 宿主 cc | `dynamically linked`，`(NEEDED) libc.so.6` | 0 |
| `target_clones`-only（无 libc） | 内置链接器 | `statically linked`，`readelf -d` 无 dynamic 段 | 0（可运行） |
| `target_clones` + `printf` | 内置链接器 | **链接失败**：89 条 E0302（getline/fopen/vprintf…） | 非 0 |

---

## 二、方向选择与理由

任务书限定走第二方向（内置链接器产出真正可动态链接的产物）。**先测其可行性再决定**：

- `elf_exec.rs` 会写 PT_INTERP/PT_DYNAMIC，但 `dynamic_linker` 仅在 `output_kind` 为
  PIE/Shared 时才设（`driver.rs:2724-2727`）。Executable 路径恒无 PT_INTERP → 静态。
- 直接把 IFUNC 改道切到 PIE 也**产不出动态可执行**：实测 `dcc -O0 -pie pp.c`（printf-only）
  同样 E0302 失败（89 条）。`-pie -v` 里连 `dcc-link: auto-resolved N libc symbols`
  都没打印，说明 `driver.rs:2761` 的 libc 自动解析（`dso_cache::resolve_and_load("c")`）
  在无 `-L` 搜索路径时**返回 Err**，未把 libc 符号并入 `allowed_undef`；且 `allowed_undef`
  机制只压错误、**不会为未定义 libc 符号生成 JUMP_SLOT/GLOB_DAT + GOT/PLT 动态重定位，
  也不写 DT_NEEDED libc.so.6**。
- 契约 2 要求「IFUNC 由 IRELATIVE 在启动时解析」。`src/link/ifunc.rs` 有 `IfuncTable` /
  `gen_irelative_relas`（写 `R_X86_64_IRELATIVE`）的**骨架**，但文件头注释自陈
  「静态可执行模式下 dcc 不执行 resolver，只记录解析关系」，且需要**先有 resolver 体**
  （实测 `.o` 里没有），IRELATIVE addend 才指向得到；现下无 resolver、无 PT_INTERP、
  无 crt/_start，机制未接通。

**结论**：在 100 分钟预算内，让内置链接器产出**真正能运行的动态 target_clones 可执行文件**
需要同时补齐：①未定义 libc 符号的 DT_NEEDED + 动态重定位发射；②可运行的 PIE/ET_DYN 起步
（crt/_start 或复用宿主 crt）；③target_clones resolver 体落地 + IFUNC→IRELATIVE 接通。
这三块任一块都不是点改，且都要过内核构建那条内置链接路径的回归。**不可行**。

按契约优先级与「严禁停在 TODO」的要求，本包**交付契约 1（底线，已完成）+ 契约 3
（把缺口写成可证伪边界）**，**不**翻转三态测试（未产出动态产物，不满足翻转前提）。

---

## 三、改动清单

只动 `src/driver.rs`（所有权内）+ 在 `tests/integration/c_link_debug.rs`（独占）加判据。

1. `run_with_host_link` IFUNC 改道分支（`src/driver.rs` 约 3017-3063）：
   把原先**仅 `-v` 才打印**、且**不说链接方式**的一行，改为**无条件**三段诊断：
   - 改道前：`dcc: note: ... contains STT_GNU_IFUNC symbols ... the host linker (ld)
     cannot resolve dcc's IFUNC resolvers, so the built-in linker is used ...`
   - 链上（Ok）：`dcc: warning: ... was linked ... as a STATIC executable (no PT_INTERP /
     DT_NEEDED); ... downgraded to static. Known defect CF-IFUNC-DYNLINK (docs §7.38).`
   - 链失败（Err）：`dcc: note: the built-in linker ... cannot emit a dynamically-linked
     executable ... The undefined-symbol errors below follow from that diversion, not from
     a missing libc symbol table. CF-IFUNC-DYNLINK.`
2. `run_builtin_linker` 写出行（`src/driver.rs` 约 2851-2864）：按 `link_cfg.dynamic_linker`
   是否为 `Some` 显式标注 `static (no PT_INTERP / DT_NEEDED)` 或
   `dynamic (PT_INTERP + DT_NEEDED)`，使**任何**走内置链接器的产物都在输出里自报链接方式。

契约 4 落实：诊断文本与 `file`/`readelf -d` 实测一致（成功侧确为 `statically linked`，
无 `(NEEDED)`；见判据 `target_clones_static_downgrade_is_announced_not_silent` 里对产物
的复核断言），不说谎、不默默无效。

---

## 四、判据与实测

### 4.1 手动实测（新二进制 `tgt_ifunc/release/dcc`）

纯 target_clones（无 `-v`）：
```
dcc: note: .../dcc-hostlink-tc_only.c.o contains STT_GNU_IFUNC symbols ... built-in linker is used ...
dcc: built-in linker wrote tcA (static (no PT_INTERP / DT_NEEDED), 9720 bytes, Executable)
dcc: warning: tcA was linked ... as a STATIC executable ... downgraded to static. ... CF-IFUNC-DYNLINK ...
file tcA → ELF 64-bit LSB executable, ... statically linked, not stripped
```
clones + printf（无 `-v`）：
```
dcc: note: ... contains STT_GNU_IFUNC symbols ... built-in linker is used ...
dcc: note: the built-in linker ... cannot emit a dynamically-linked executable ...
        The undefined-symbol errors below follow from that diversion ... CF-IFUNC-DYNLINK.
（其后 89 条 E0302）exit=1
```

### 4.2 cargo 判据（`tests/integration/c_link_debug.rs`）

- `target_clones_static_downgrade_is_announced_not_silent`：成功链上纯 clones，断言 stderr
  含改道原因（`STT_GNU_IFUNC` + `built-in linker`）与静态后果（`static` + `downgrade`），
  并用 `file`/`readelf -d` 复核产物确为 `statically linked` 且无 `(NEEDED)`。
- `target_clones_libc_link_failure_is_attributed_to_diversion`：clones+libc 失败，断言
  失败被归因到 IFUNC 改道（`STT_GNU_IFUNC`+`built-in linker`+`CF-IFUNC-DYNLINK`），而非
  让用户面对孤立的 E0302 墙。
- 回归：既有三态 `target_clones_diverts_to_static_link_is_known_defect`（在
  `c_attr_builtin.rs`，非独占文件）保持绿（clones 仍链不上→None 分支）。

（`cargo test` 结果见文末「六、执行记录」。）

---

## 五、仍未闭合的缺口（可证伪的边界）

契约 2「真修」未闭合，缺口具体到 ELF 构件与代码位置：

1. **resolver 体缺失**（根因）：`.o` 里 `compute`/`multiply` 是 IFUNC 类型但 `st_value`
   指向实现体、无 CPUID resolver。真修需在 codegen 发生成 resolver（`src/codegen/**`
   后端，本包禁改），并让 `__builtin_cpu_supports` 真实现（现仅 `preproc.rs` 名字表，
   无实现，亦禁改）——这是 §7.38 说的「第一方向」，本包不越界。
2. **未定义 libc 符号的动态化未接通**：`src/driver.rs:2759-2783` 的 libc 自动解析
   （`link::dso_cache::resolve_and_load("c")`）在无 `-L` 时返回 Err；且
   `src/link/resolve.rs` / `relocate.rs` 里 `allowed_undef` 仅抑制 E0302，**不产生**
   未定义符号的 `JUMP_SLOT`/`GLOB_DAT` GOT 项与 `DT_NEEDED libc.so.6`。缺这三者，
   任何走内置链接器的动态产物运行时必炸。最小切口：在 `elf_dyn.rs`/`elf_exec.rs` 的
   `.dynamic` 生成里补 `DT_NEEDED`，并在 `plt_got.rs` 为每个「允许未定义」的外部函数
   发射 PLT 桩 + GOT + `R_X86_64_JUMP_SLOT`。
3. **PT_INTERP / dynamic_linker 仅 PIE/Shared**：`src/driver.rs:2724-2727`。若走
   ET_EXEC 动态，需要给 Executable 也设 `dynamic_linker`；若走 PIE 动态，需要 crt/`_start`
   起步（现 Executable 用 `entry="main"`，PIE 用 `"_start"` 但无 crt 对象）。
4. **IFUNC→IRELATIVE 未接通**：`src/link/ifunc.rs::gen_irelative_relas` 有骨架但
   `resolvers` 表由 `.dcc.ifunc` 节填充（`collect_from_bundle`），而 codegen 未写
   `.dcc.ifunc`（实测 `.o` 无该节）。addend 需要的 resolver_vaddr 无从取得。

**建议下一步最小可行切口**：先做 (2) 的一半——让内置链接器在 PIE 模式下把未定义 libc 符号
并入 DT_NEEDED + JUMP_SLOT（不碰 IFUNC），用 printf-only `-pie` 打通「内置链接器产出可运行
动态可执行」这条端到端；再在其上把 (1)/(4) 的 resolver + IRELATIVE 接上。两块都不越界到
codegen，但都触及内核构建用的内置链接路径，须由集成阶段跑 `scripts/kernel_full_check.sh`
回归。

---

## 六、执行记录

- 构建：`CARGO_TARGET_DIR=.work_tmp/tgt_ifunc cargo build --release -j1` → Finished（3m50s）。
- 判据运行：
  ```
  CARGO_TARGET_DIR=.work_tmp/tgt_ifunc nice -n 19 cargo test --release -j1 \
      --test integration target_clones -- --test-threads=1
  Finished in 6m03s; running 7 tests:
    attr_target_clones_compile_and_run ........................ ok
    attr_target_clones_ifunc_symbols ........................... ok
    target_clones_diverts_to_static_link_is_known_defect ....... ok   （既有三态，未翻，仍绿）
    target_clones_libc_link_failure_is_attributed_to_diversion . ok   （本包新增·契约1失败侧）
    target_clones_resolver_code_generation ..................... ok
    target_clones_resolver_vaddr_patching ...................... ok
    target_clones_static_downgrade_is_announced_not_silent ..... ok   （本包新增·契约1成功侧）
  test result: ok. 7 passed; 0 failed; 1015 filtered out
  ```
- 跨所有权输出字符串依赖核查：`grep -rn 'built-in linker wrote|via builtin linker, IFUNC|
  using builtin linker|IFUNC symbols detected' scripts/ tests/ Makefile` → 无命中，
  改动的 stderr 文本不被任何脚本/测试解析，产物字节不变（仅日志行加了 static/dynamic 标注）。

### 需要负责人代做 / 留意的跨所有权事项

1. 若日后接通契约 2 使 target_clones 产出**动态**产物：须把本包**没有翻**的既有三态测试
   `target_clones_diverts_to_static_link_is_known_defect`（`tests/integration/c_attr_builtin.rs`，
   **非本包独占文件**）与本报告两条新增判据的 `Some(false)`/`!ok` 断言一并翻成「断言动态」。
2. `src/driver.rs:2717-2718` 注释「codegen 已生成 resolver 和 STT_GNU_IFUNC 符号」经实测
   **半假**（resolver 未生成）。本包已改的是改道分支代码，未动这条注释——如要更正其措辞
   属 `src/driver.rs`（本包可改，但为把改动面压到最小、避免与并行工作流在同文件摩擦，
   建议由集成阶段一并订正）。
