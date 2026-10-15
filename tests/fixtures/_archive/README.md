# tests/fixtures/_archive/

> Historical fixture directory — files with **no test reference at all**, kept
> only so blame history stays readable. **DO NOT** reference anything here from
> any test or script: `scripts/check_fixtures.sh` treats every such reference as
> a hard failure (`FORBIDDEN-ARCHIVE-REF`; the rule lives in
> [`../README.md`](../README.md) §3 引用完整性 and §5 零引用件的处置判定标准).
> There is no scheduled clean-up and no phase plan behind this
> directory — the rule set above is the end state.

## Rules

* **No** `include_str!`, `Path` join, or shell reference may point into this
  directory. `scripts/check_fixtures.sh` enforces this as a hard failure.
* New fixtures that you might think belong here should instead be placed in
  the active directory that matches the language and role, per
  `tests/fixtures/README.md` §目录=语言/角色.
* If a fixture is moved here from `tests/fixtures/{c,cpp,…}`, the move is
  expected to be `git mv` so blame history is preserved, plus one line in the
  table below recording why it stopped being referenced.

## Current contents

| Sub-dir  | Origin        | Reason | Future |
|----------|---------------|--------|--------|
| `dev/`   | `fixtures/dev/` | 88 files with zero `.rs`/`.sh` references (D1, 2026-09-28) | Deletable after one milestone with no docs reference |
| `asm/`   | `fixtures/asm/` | `asm_multi.c` —— 唯一内容是 `asm volatile("…" : "=r" (0) : "g"(1),"r"(2) : "cc")`，输出操作数为**非左值字面量 `0`**，gcc 直接拒绝（`error: lvalue required in 'asm' statement`），因此根本没有 oracle 可钉；且它想覆盖的三个面（cc clobber / "g" 约束 / 多语句模板）已由 `asm_clobber.c`、`asm_constr.c`、`asm_ext.c`、`asm_extended_constraints.c` 的绿灯用例分别承担 —— 语义重复且无差分能力。 | Deletable |
| `c/`     | `fixtures/c/` | `riscv_rvv_ops.c` —— 夹具自己的注释就写着“此测试验证基本标量浮点和整数运算”，正文无任何向量类型/intrinsic；而 dcc 根本没有 RVV lowering：`--target=riscv64-linux-gnu -O0 -S` 输出里 `vsetvli/vsetivli/vle/vse/vmv` 计数为 **0**，函数体只有占位注释 `// IR lowering, 22 bytes` + `ret`（riscv64 的 `-S` 路径不发指令文本）。改走 `-c` 后 `riscv64-linux-gnu-objdump -d` 又暴露编码缺陷：`add_ints` 里的 `552e` 被解成 `lw a0,232(sp)`（应为 `add`），`mul_ints` 发的是 `sll a0,a0,a1`（应为 `mul`）。任何“按夹具名断言 RVV/正确标量指令”的用例都只能钉死缺陷，按 §7.4-A 第三档（无真实判据可写）归档。 | Re-add once RVV lowering lands |
| `c/`     | `fixtures/c/` | `incbin_test.c` —— 判据要求是“`.incbin` 嵌入的字节真的出现在产物节数据里”，但 dcc 集成汇编器**静默丢弃 `.incbin`**：两个 `.section .rodata.incbin_*` 节头存在、`SHF_ALLOC` 也在，`sh_size` 恒为 **0**；用绝对路径指向一个确实存有的 23 字节 `/tmp/ib.bin` 同样为 0（对照：同节内 `.string "GHIJKL"` 能落盘，`.section`+`.string` 单独用可落 7 字节）；`-S` 模式下文件级 `__asm__` 整块从输出里消失。根因侧：`src/codegen/x86_64/asm/directive_emit.rs` 的 `.incbin` 分支在这条路径上未生效，且 `fs::read` 失败被 `if let Ok` 吞掉——无诊断。`src/**` 不在本次任务可改范围，无法写正例断言（也无法写“具体诊断码”的 xfail，因为它不报错），按 §7.4-A 第三档归档；配套数据文件 `c/incbin_data.bin` 留在活动目录，供 `.incbin` 真正实现后直接复用。 | Re-add when `.incbin` emits bytes |
| `c/`     | `fixtures/c/` | `builtin_lowlevel.c` 的**严格子集**（原名 `m27_lowlevel_simple.c`，S6 改名时判定）：34 行只包含 `assume_aligned` / `alloca` 两个 builtin，而在用的 `c/builtin_lowlevel.c`（52 行）除这两项外还覆盖 `frame_address` / `return_address`，且已有 `m27_lowlevel_builtins_runtime` 在 `c_attr_builtin.rs` 里对它做产物+运行断言。按 §7.4-A 第三档（与现有件语义重复）归档，避开只为“零引用计数”而新建一个重复测试。 | Re-add only if a standalone minimal lowlevel TU is needed |

## How to un-archive a fixture

1. Move the file from `_archive/<orig>/<file>` back to `c/`, `cpp/`, `asm/`
   or other active directory via `git mv`.
2. Add the test that references it (or re-attach the original reference).
3. Run `bash scripts/check_fixtures.sh` to verify the move is consistent.