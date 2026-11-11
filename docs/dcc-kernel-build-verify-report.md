# DCC — Linux 7.1.0 (localmodconfig, x86-64): build & verification results

**Status (2026-11-11).** The full-tree build, the objtool/ELF checks, the realmode comparison
and the runtime/module closure are green, and the stress round is green as well: with `-smp 4`
plus a virtio-net device the dcc-built kernel and the gcc-built baseline print the same
**26** `RT|`/`MOD|` lines (`scripts/runtime_closure.sh --stress`: 7 passed, 0 failed). The
4-vCPU init stack runs close to its guard page; the measured rate is in the first bullet of §7. Every
object in `/root/pvh-lmc3` was produced by the current compiler identity — the audit of all
4 612 `.cmd` files finds no gcc command line (§2). §5.59, a runtime `offsetof` defect that the
kernel itself exposed, is fixed and the affected objects were rebuilt.

## 1. Sources and environment

| Item | Value |
|---|---|
| dcc | this repository at 2026-11-12 (§5.59 + §5.60); `cargo build --release -j2` → `target/release/dcc` |
| Kernel | Linux 7.1.0, pristine source tree `/root/linux-pvh-src` (no git metadata, no local patches) |
| Config | `localmodconfig` seeded from `defconfig`, then `PVH=y`, `SUSPEND=n`, `HIBERNATION=n`, `ACPI_SLEEP=n`, `IA32_EMULATION=n`, `COMPAT=n` |
| Build dir | out-of-tree `O=/root/pvh-lmc3` (3 005 objects, 4 612 `.cmd`); after the cold round only incremental rounds — no `make clean` |
| Host | Linux x86-64, 4 cores; `gcc 13.3.0` is used only as a comparison baseline in `/root/pvh-gcc-rt`, never as a build compiler; `scripts/runtime_closure.sh --audit` proves it |
| Emulator | QEMU 8.2.2 (`-kernel` PVH entry, `-m 1024M`, serial console) |

`.config` has 1 577 `=y` and 1 `=m` symbols; the build tree holds 3 005 object files. The same
`CC` runs at configure and build time, so `CONFIG_CC_VERSION_TEXT` stays `dcc 0.1.0`. kbuild
therefore also records `CONFIG_CC_IS_CLANG=y` (dcc predefines `__clang__`); that is consistent
rather than contradictory, because the clang-shaped `-f…`/`-m…` flags kbuild then derives are
exactly the ones on every dcc compile line, and dcc documents which of them it implements
(`docs/support_features.md`).

## 2. What "all-dcc" means here

The build is driven through `scripts/dcc-kernel-cc.sh` / `scripts/dcc-host-wrapper.sh`
(`CC=` / `HOSTCC=`), which hand every C/asm compile to `target/release/dcc`. Two independent
checks make the claim falsifiable:

* `scripts/kernel_pvh_verify.sh --build` prepends a trap directory to `PATH` containing `gcc`,
  `cc`, `g++`, `c++`, `clang`, `ld.bfd`; each trap appends its command line to a log and exits
  99. **0 trap hits** for this build.
* the same script counts `GCC-DELEGATE` lines in the build log (the wrapper used to fall back to
  gcc for a few infrastructure files). **0 delegate lines.**
* `scripts/runtime_closure.sh --audit` reads the first token of every `savedcmd_` line in the
  tree's `*.cmd` files. A gcc command line there means an object was compiled by gcc inside the
  dcc tree; the current tree reports 4 612 `.cmd`, none with a gcc first token. The check looks
  only at the first token because the `gcc` substring is on almost every line (`-I/usr/lib/gcc/…`);
  a substring check produced 565 false positives. The audit compares the recorded command line,
  not the dcc binary version; objects from an older dcc build are tracked by
  `scripts/kernel_purge_stale_objects.sh` instead.

Host `ld` still does the final link — dcc emits relocatable objects and drives the system linker
directly; it is not a linker. That is the same split as a gcc build (`gcc` → `as` → `ld`), with
dcc replacing `cc` and `as` for the compilation steps.

## 3. Build

```bash
KERNEL_TREE=/root/linux-pvh-src BUILD=/root/pvh-lmc3 \
  bash scripts/kernel_pvh_verify.sh --config
KERNEL_TREE=/root/linux-pvh-src BUILD=/root/pvh-lmc3 DCC_BIN=target/release/dcc JOBS=2 \
  bash scripts/kernel_pvh_verify.sh --build
```

Measured on the cold-cache clean round (an earlier build directory; the current tree only
received incremental rounds):

| Metric | Value |
|---|---|
| `CC` lines in the build log | 2 901 |
| object files in the tree | 2 998 (cold round; 3 005 after the later incremental rounds) |
| wall time, `-j2` on 4 cores | 3 h 52 min |
| `vmlinux` | 171 212 384 B |
| gcc traps / delegate lines | 0 / 0 |
| objtool errors | 0 |

`make vmlinux` does not build `arch/x86/boot/bzImage`; the PVH route uses `vmlinux` directly.
The empty-tree rebuilds after each fix round (`-j3`) took 1:21–2:34 and produced
171 245 152–171 376 200 B images.

After each compiler fix the cycle was an incremental one — rebuild the affected subsystems,
relink, boot — never a clean rebuild. The round that carries §5.48–§5.50 recompiled 2 937
objects (`-j4`, 2 h 50 min) and produced a 164 796 160 B `vmlinux` with 0 objtool errors;
the two rounds after it touched 85 and 319 objects.

## 4. Verification results

### 4.1 objtool

objtool runs as part of the kbuild link path for every object in this configuration.
Result: **0 errors**. The only diagnostics are `return with modified stack frame` warnings
(**12 225** across the whole tree); they are warnings, not errors, and objtool and the link both
exit 0. They are not evidence of correctness either way, but they are the reason `vmlinux` links
at all: objtool's ORC pass accepts dcc's prologues/epilogues.

### 4.2 ELF-level checks

* `__jump_table`: **5 053 entries**; each entry's `code` target must point at the first byte of a
  `jmp` (`0xeb`/`0xe9`). **`bad_nop=0`, `bad_other=0`, `out_of_text=0`** — no entry points into
  a `.text` NOP hole left behind by a moved function.
* `.altinstructions`: **2 349 entries** (`0x8076` bytes, 14 B each); the nested `ALTERNATIVE_2`
  sites in `arch/x86/kernel/pvclock.o` carry the GAS-compatible `instrlen/repllen` pairs `(5,5)`
  and `(5,3)`, and `.altinstr_replacement` contains the real `lfence; rdtsc; rdtscp` bytes rather
  than NOPs.
* executable `.pushsection` blocks (`.altinstr_aux`, `.altinstr_replacement`, `.fixup`,
  `.ex_table`) carry their own `R_X86_64_PC32` relocations in `.rela.<section>`: cross-section
  jumps are emitted against the **target section symbol** with `addend = offset - 4`, matching
  what GAS produces for the same inline asm.

### 4.3 Boot (QEMU PVH)

The judge is the serial console reaching `DCC kernel: init reached successfully`;
`Run /init as init process` only proves the hand-off to user space. Recent rounds:

| image | compiler carries | first stop | diagnosed as |
|---|---|---|---|
| kbuild18, empty tree | §5.14–§5.36 | SMP up, `kdevtmpfs` running (2.757 s), then `#PF` walking an empty `hlist_nulls` bucket | §5.37 |
| incremental, image carries §5.37 | +§5.37 | past the `hlist_nulls` walk, then `#PF` at `dev_net(NULL)` | §5.38 |
| incremental, image carries §5.38 | +§5.38 | reached the `serial8250` initcalls, `TASK stack guard page was hit` at 2.048 s (16 KB init stack) | §5.39 |
| kbuild19, empty tree | §5.14–§5.39 | did not link: `arch/x86/kernel/fpu/bugs.c` died with `E0311` (four x87 memory operands had no encoder) | §5.40 |
| kbuild20, empty tree | §5.14–§5.40 | **hang** right after `mem auto-init: stack:all(zero), heap alloc:off, heap free:off` (14:35, QEMU killed by the 45 s timeout). gdb on the stub: `__free_pages_prepare(page=0xffffea0000000000, order=0, …)` entered again and again from `__free_pages_core+0x246` — PFN 0 re-freed forever inside `memblock_free_all()` | §5.41 |
| kbuild21, empty tree | §5.14–§5.41 | `BUG: TASK stack guard page was hit at ffffc9000000ffe8` at 4.002 s, `RIP: decay_load+0xc` (`sub rsp,0xa0` then the first `push`), RSP `ffffc9000000fff8` — the 16 KB task stack was **8 bytes short** at that instant; the chain below is the scheduler's `decay_load → ___update_load_sum → __update_load_avg_se → set_next_entity → pick_next_task_fair → __schedule` | §5.42 (frames) |
| kbuild22, empty tree | §5.14–§5.42 | `loop_init → loop_add → device_add_disk`: `#PF` at `add_disk_fwnode+0x6c`, address `0x10`. gdb on the stub: `__alloc_disk_node` stored the queue at `disk=0xffff888000b96400 +0x50`, but `add_disk_fwnode` was entered with `disk=0xffff888000b96560` (+0x160) and `queue=(nil)` — `__blk_mq_alloc_disk` returned `disk+0x160` | §5.44 |
| kbuild23, empty tree | §5.14–§5.44 | past the loop block-device registration, then `#PF` at `ata_lookup_timeout_table+0x62`, address `0xa1ec`, `Comm: scsi_eh_1` (first libata EH command); a later `general protection fault … non-canonical address 0xfffc43ff90eab168` in `seq_create_client1+0x78f` follows the first Oops — a **separate** defect, `clienttab[client->number = c] = client` scaling the lvalue address, see §5.47 — and init dies with `Attempted to kill init! exitcode=0x0000000b`. The `ata_eh_cmd_timeout_table[].commands` slots held `{0xec,0xa1}` as an integer (0xa1ec) with no relocation | §5.46, §5.47 |
| kbuild24, empty tree | §5.14–§5.47 | three Oops on the boot path: `__ata_scsi_queuecmd+0xb3d` (4-byte write to `0x5c`), `alloc_thread_stack_node+0x8f` (`CR2=0x2d`), `scsi_eh_scmd_add+0x445` (`CR2=0xe0`) | §5.48, §5.49 |
| full rebuild carrying §5.48–§5.49 | +§5.48, §5.49 | `__ata_scsi_queuecmd` still faults, now on `rq->tag` read at offset 0 (`*(int*)rq`) | §5.50 |
| incremental (85 TUs: `block`, `ata`, `scsi`, `cdrom`) carrying §5.50 | +§5.50 | **`Run /init as init process` at 10.16 s** — first image past the whole boot path; `/init` then fails to exec with `-7` | open |
| incremental (`fs`, `init`) carrying all of the above | same | `Run /init as init process` at 10.42 s, `/init` exec returns `-7` again | open |
| full rebuild (kbuild26) carrying §5.51–§5.57 | +§5.51–§5.57 | `/init` runs: 8 of the 9 `RT\|` tests pass, `RT\|ext4\|FAIL:mount` — `JBD2: no valid journal superblock found`; the whole `MOD\|` panel is green | §5.58 |
| incremental (46 TUs: `fs/jbd2`, `fs/ext4`) carrying §5.58 | +§5.58 | `RT\|ext4\|magic roundtrip umount`, `RT\|exit\|0`; all **21** `RT\|`/`MOD\|` lines (the panel size at that time — the stress layer added five, §4.3 last row) are byte-identical to the gcc-built kernel (same `.config`, `scripts/runtime_closure.sh --compare`: PASS 21/21) | — |
| full rebuild (kbuild27, 2 922 CC, one compiler identity) carrying §5.51–§5.58 | +§5.51–§5.58 | `Run /init as init process` → `RT\|basic` … `RT\|ext4\|magic roundtrip umount`, `RT\|exit\|0`, `RT\|status\|0`; `configs.ko` and `hello.ko` rebuilt by the same compiler (`insmod`, proc read, `lsmod`, `dmesg`, `rmmod`, resident) → `MOD\|done`; the 21 `RT\|`/`MOD\|` lines are again byte-identical to the gcc-built kernel | — |
| incremental 2026-11-11 (fib_trie, arp, eth, virtio-net, kernel/{events,cgroup}, `*_sockglue`, …) carrying §5.59/§5.60 | +§5.59, §5.60 | `-smp 4` + virtio-net: `RT\|smp\|online=4`, `RT\|sched\|8x20000=160000`, `RT\|threads\|4x20000=80000`, `RT\|net\|eth0 arp-complete`, `RT\|blk\|256KiB fsync 4x64KiB`, `RT\|exit\|0`, `RT\|status\|0`; all **26** `RT\|`/`MOD\|` lines byte-identical to the gcc-built kernel (`--stress`: 7 passed, 0 failed, 0 panic on either side) | — |

The `WARNING … kernel/module/main.c:1723 at __layout_sections` printed when `configs.ko` is
inserted is **not** a dcc diagnostic: the gcc-built kernel prints the same warning
(`+0xf5/0x110` there, `+0x288/0x398` here), it does not stop the module from loading, and the panel
lines around it are identical.

Image identity is checked from the image itself: the `Linux version 7.1.0 (root@di) (dcc 0.1.0,
GNU ld … 2.42)` string in `.rodata` is what proves a `vmlinux` is dcc-built (a `gcc …` string
there means it is not).

### 4.4 Realmode assembly (`--partial`)

The six `.S` files that end up in `rmpiggy.o` are assembled by dcc with `-m16` and compared
against `gcc -m16 -c` after `objcopy --strip-debug`: code, relocations and symbols are
**byte-identical** (6/6). The `-m16` **C** inputs still fail with a hard `E0319` diagnostic plus a
`DCC-GAP reason=m16/m32-no-c-backend` marker instead of silently emitting 64-bit code, and the gcc
trap stays empty (14 passed, 0 failed). This stage has to run after `--build`: the six files
include build-generated headers (`generated/autoconf.h`, `generated/asm-offsets.h`).

### 4.5 dcc's own test cases

Both targets were re-run on the final compiler carrying §5.59 + §5.60 (2026-11-12):
`cargo test --release --test integration -- --test-threads=2` logged
**integration 1 375 passed, 0 failed** (6 ignored, 66.7 s), and
`cargo test --release --lib -j2 -- --test-threads=2` logged
**lib 2 612 passed, 0 failed** (2 ignored, 1.6 s, log `logs/cargo_test_lib_final.log`). The two
integration cases added since the previous full run are the §5.59 `offsetof` and §5.60
nested-guard fixtures. The previous full run (2026-11-07, compiler of §5.18, log `logs/cargo_test_ssa_width.log`) logged
**4 054 passed, 8 ignored, 1 failed** — lib 2 607/0 (two ignored), integration 1 332/0 (six
ignored), `errors` 26/0, `disasm_snapshot` 1/0, `g10_snapshot_baseline` 1/0. The single failure
is the pre-existing `cpp_abi_eh_group` (six C++ zero-cost-EH fixtures die with a signal;
identical before this cycle and not on the kernel path). Every defect in §5 carries a regression
case, most of them a C fixture checked against gcc at `-O0/-O1/-O2/-O3`.

Product fingerprints were re-frozen when frame layout legitimately moved: 34 disassembly
snapshots under `tests/disasm_snapshots_x86_64/` and the five `tests/fixtures/g10_*.sha256`
baselines. With numbers and padding normalised away, all 34 instruction streams hash identical
to their previous content, so only frame sizes moved (§5.12).

`scripts/torture_test.sh all` (the pre-commit test-case script): **3 707 passed, 0 failed,
6 skipped** (`logs/torture_bracefix.log`, 2026-11-07 09:04 — i.e. the compiler before
§5.17/§5.18).

On the compiler carrying §5.48–§5.50 the lib suite is green again (**2 610 passed, 0 failed**)
and the three new cases (`statement_expression_has_its_own_scope`,
`inline_param_uses_declared_param_type`, `inline_result_uses_declared_return_type`) pass at all
four `-O` levels. Those three are inline-source cases rather than `tests/fixtures/` files: each
one is a 20-line shape of the kernel expansion that failed, and the third needs a `noinline`
control function to contrast against.

The compiler carrying §5.51 adds `bitfield_init_list_uses_rmw_band`
(`tests/fixtures/c/bitfield_init_list_slots.c`), and the 12-name `bitfield` filter of the
integration suite passes as a group.

New cases in the §5.52–§5.58 round:
`codegen::x86_64::insn::tests::bswap_reg_basic` (byte encodings `41 0F C8` / `41 0F CD` / `49 0F C8`
plus the `gas_text`), `const_eval::tests::eval_int_lit_strips_suffix_in_every_radix`, and the fixture
`tests/fixtures/c/bswap_high_reg_rex_b.c` cross-checked against gcc at `-O0/-O1/-O2/-O3`
(`c_bswap_high_reg_rex_b_gcc_cross_check`).

§5.59 adds `c_offsetof_runtime_subscript_gcc_cross_check`
(`tests/fixtures/c/offsetof_runtime_subscript.c`), also run at all four `-O` levels against gcc.

§5.60 adds `nested_guard_same_name_slots_distinct_matches_gcc`
(`tests/fixtures/c/nested_guard_slots_distinct.c`): two- and three-level nested for-init guards,
a for-init guard shadowing a block local, and two guards on the same object, at all four `-O`
levels against gcc.

User-space suites on the fixed compiler (`scripts/bench_lua_sqlite.sh`): Lua builds and runs
identically to gcc at **all four** `-O` levels (same functional output, same `fib(32)` result),
and `shell.c + sqlite3.c` at `-O2` builds, links and answers the `:memory:` probe correctly
(7 763 584 B binary, 7 min 17 s, 6 973 MiB peak RSS). The `-O2` SQLite peak is a pre-existing
memory cliff of this host (6 774 MiB / 7:33 here against HEAD's 6 785 MiB / 7:43 on the same
single TU); the layout work neither added memory nor time.

## 5. Defects fixed in this cycle

### 5.1–5.10 fixed, each with a regression case

| # | Symptom | Root cause | Fix |
|---|---|---|---|
| 1 | `#pragma pack` state lost when a header came from the include cache | cache key did not cover/restore pack state | cache V9 + state replay; cold/hot two-pass `sizeof` case |
| 2 | `__jump_table` entries pointed into `.text` NOP holes | labels in functions moved to custom sections were still resolved against `.text` | label sectioning via `pushsection_code_ranges` / `custom_func_move_ranges` |
| 3 | `objtool: weirdly overlapping alternative! 8 != 6` (`pvclock.o`) | nested `ALTERNATIVE_2`: the `.skip` measurement pass produced a stale coordinate (2 instead of 5) | `eval_asm_label_expr` prefers the current pass's coordinate |
| 4 | `.altinstr_replacement` contained only NOPs | whole functions were resolved before executable `.pushsection` blocks | move executable `.pushsection` ranges first, then functions |
| 5 | `objtool: can't find jump dest instruction at .altinstr_aux+0x2c` | a function range and a nested `.altinstr_aux` range both contained the jump; first match won | pick the **innermost** containing range |
| 6 | cross-section jumps inside moved blocks had no relocation | the symbol was the `.L` label, which is not in `.symtab` | emit `R_X86_64_PC32` against the target **section symbol**, `addend = offset - 4` |
| 7 | pushsection relocations landed in `.rela.init.text` at NOP-hole offsets | `emit_rel_relocations` used the same first-match rule | same innermost-range rule when migrating relocation offsets |
| 8 | `testb $1, boot_cpu_data+N(%rip)` read the neighbouring capability byte | PC32 addend was `-4`, but the immediate follows `disp32`, so RIP is `P+5` | `addend = -4 - imm_size` (`-5` for `testb`, `-8` for `testl`) |
| 9 | PVH boot died at `native_apic_mem_read`, APIC fixmap PTE `0xFFFFFFFF87DFF398` | a block-scope 8-byte struct initialised from a compound literal stored the **buffer address** as the value | `needs_compound_copy` in all three declaration paths |
| 10 | `complex-7.c` fails to compile with `E1128 frame budget exhausted` once the constant 512 B vector band is dropped | `sret_bytes_expr` never registered the 32-byte `_Complex long double` slots and summed `Binop` subexpressions as a maximum | `complex_ld_slot` + operand/argument/leaf accounting; `Binop` sums both sides |

### 5.11 `"+m"` output bound to the `"=@ccz"` slot at `-O2`

**Reproducer** (`scripts/kernel_cc_asm_repro.sh`, copied from
`arch/x86/include/asm/rmwcc.h`'s `__GEN_RMWcc`, where the condition-code output is
*invisible* — the template never mentions it):

```c
static inline bool try_cmpxchg(int *v, int old, int new)
{
	bool c;
	asm volatile("lock cmpxchgl %[new], %[var]"
		     : [var] "+m" (*v), "=@ccz" (c)
		     : [new] "r" (new), "a" (old) : "memory");
	return c;
}
```

| | `-O0` | `-O2` (before) | `-O2` (after) |
|---|---|---|---|
| gcc | `fail=0 ok=1 v=3` | `fail=0 ok=1 v=3` | `fail=0 ok=1 v=3` |
| dcc | `fail=0 ok=1 v=3` | **`fail=0 ok=0 v=5`** (rc 1) | `fail=0 ok=1 v=3` (rc 0) |

Both causes are in the `-O2` SSA round trip, not in the operand binder: `output_vars` was pushed
only for outputs whose operand is an `Expr::Ident`, so it was misaligned with `outputs` and the
`"=@ccz"` write variable moved onto the `"+m"(*v)` slot; and an asm output version was invisible
to the version census behind constant propagation, which then deleted the store that produced the
old value. `output_vars` is now a positionally aligned `Vec<Option<SsaVar>>` (rebuild through
`local_splits::ast_name`) and counts as a write. Two outputs that resolve to the same storage are
now a hard error (**E0321**) instead of silent codegen, except match-digit outputs (`"0"`/`"1"`).

### 5.12 Frame economy: 240 B of the 288 B unconditional floor is gone

Same 120 objects (`kernel/`, `mm/`, `lib/`), prologue `sub rsp` per function:

| compiler / stage | most common prologue frames |
|---|---|
| gcc | `0x8`, `0x10`, `0x18`, `0x20`, `0x28`, `0x30` |
| dcc before | `0x140`, `0x150`, `0x160`, `0x170`, `0x180`, `0x190` |
| dcc after | `0x20`, `0x60`, `0x30`, `0x50`, `0x70`, `0x90` |

The modal non-trivial frame went from **320 B (`0x140`, 795 sites)** to **96 B (`0x60`, 815
sites)**; `0x20` — what a leaf with no locals costs — is now the largest single bucket (1 452
sites). What remains is the floor this cycle did not remove: `FIXED_HDR` 48 B + `temp_slots ≥ 2`
16 B, i.e. a function with one int local closes at `0x50`–`0x60` where gcc sits at `0x8`–`0x50`.

`CX_FRAME_BAND` 48 B, `BF_RMW_RESERVE` 64 B and `stack_reserve` 128 B are all demand-driven now.
That required laying the frame bottom out as a cursor of disjoint ranges
(`BOTTOM_PAD → cx → va → sc → BOTTOM_PAD → vec → bf → trampoline`, pinned by
`bottom_bands_own_disjoint_ranges`), after which six upstream accounting holes that the flat
padding had been absorbing had to be fixed at their sources (VLA offset scratch, `Switch` sret
slots, `Stmt::Label`/`Invoke`/`EhLandingPad`/`AsmGoto` charged 0, both arms of a branch summed
instead of `max`, `tramp_top` missing `tramp_reserve`, `vec_temp_off`'s literal `+8`). It also
fixed a live defect: the bitfield band was a fixed 64 B that fit two layers, while
`bitfield_rmw_deep_nest.c` nests nine and had been writing 224 B past its own band.

Frame size is not what holds user-space speed back: after this work Lua `fib(32)` best-of-3 is
284 ms against gcc's 116 ms (≈2.45×, unchanged from the 2.4× in
`docs/dcc-lua-sqlite-build-verify-report.md`). What this buys is kernel stack headroom.

### 5.13 Optimization pipeline health check: what the `-O2` step actually buys

**Wall time** (release dcc, 60 random `gcc.c-torture/execute` units, median of 3):

| level | total |
|---|---|
| `-O0` | 984 ms |
| `-O1` | 1 018 ms (**1.03×** `-O0`) |
| `-O2` | 3 253 ms (3.3×) |

**Hit rate at `-O2`** — compile with `DCC_OPT_SKIP=<pass>` and compare `.o` bytes:

| pass | torture (natural) | opt fixtures |
|---|---|---|
| `run_ssa_passes` | 49/60 | 52/60 |
| `ssa_regalloc` | 23/60 | 45/60 |
| `cross_function_dce` | 7/60 | 17/60 |
| `function_inline` | 6/60 | 16/60 |
| `fold_constants` | 1/60 | 8/60 |
| `prune_dead_stores` | 2/60 | 1/60 |
| `ast_gvn`, `loop_opt`, `ipa_modref` | **0/60** | 4/60, 2/60, 0/60 |

Twelve of nineteen named passes change **zero bytes** on 60 real units, yet each walks the whole
AST. No single skipped pass moves the wall clock measurably (±1 ms/unit): the `-O0`→`-O2` gap is
superadditive — ~17 full traversals per TU. Where the time goes instead: the allocator is the
largest consumer (`malloc`/`_int_malloc`/`_int_free`/`unlink_chunk`/`malloc_consolidate`/`cfree`/
`memmove` ≈ **27 %**) and `Expr`/`Stmt` clone+drop ≈ **11 %**; the hottest `dcc::` routine is
`ast_gvn::walk_expr_addr_of` at 2.1 %.

Conclusions, in order of measured payoff: (1) stop cloning the AST per pass and share one
escape/alias fact set — that attacks the 27 % and the 11 % together; (2) gate the inert passes on
a cheap precondition rather than delete them — `fold_constants`, `fold_constant_branches` and
`eliminate_unreachable` are load-bearing for the kernel's compile-time-size traps even at 0–2/60
here. The numbers are corpus-bound (60 small torture units) and re-derivable with
`scripts/opt_pass_audit.sh`.

### 5.14–5.41 boot-path defects, each fixed at the compiler

Each row has a regression case in the dcc suite (fixture and case name are in the test sources).

| # | Defect | Fix |
|---|---|---|
| 14 | static initialisation recognised only the `= { … }` spelling, not the compound-literal `= (T){ … }`: `DEFINE_SPINLOCK` became `.bss` + a runtime store of the buffer address into the lock word (`early_pfn_lock`), and a file-scope `(T){…}` silently emitted zeros | one `static_brace_list` predicate at all five static-init sites; aggregate runtime init goes through `gen_static_local_init`'s block copy |
| 15 | `rvalue_is_buffer_addr` answered by size before shape, so ≤ 8 B compound literals and statement expressions were read as values instead of addresses | value model split into type axis / delivery axis; `assign_needs_whole_copy(ty, rhs)` is the only predicate |
| 16 | `__attribute__((cleanup))` at function-body scope never ran, so `guard()` unlocked nothing | `emit_func_scope_cleanups` in the epilogue, return value preserved across the destructor call |
| 17 | a folded `(u64)` literal lost its width in the SSA round trip (`put_dec_full8`: truncate to 32 bits, then shift by 32) | `bin_operand_tys` records both operand widths per instruction destination |
| 18 | `__builtin_clzll`'s width came from the operand, not the name; `1UL << 34` asked for 1 TB (sparse usemap) | width from the builtin name; `call_arg_tys` records argument AST types |
| 19 | descriptor-table register forms encoded without `mod=11` (`ltr %eax` → `ltr (%rax)`) | `0xC0` prefix on the register form; `0x66` for `str`/`sldt`; same fix for fsgsbase |
| 20 | `cpuhp_hp_states + state` (incomplete array + enum subscript) added at 32 bits | address-shaped operands are 8 bytes wide; `Type::Enum` is integer in `infer_binop_type` |
| 21 | `cmpxchg16b` encoded as `cmpxchg8b` (no REX.W) and its address scratch was overwritten by a pinned `%rbx` | pre-scan fixed-register constraints; `rex(true)` in both arms; `(%reg)` in the `-S` path |
| 22 | `__int128` member initialisers stored nothing (`emit_store` only handles 1/2/4/8 B) | `store_i128_from_rax_rdx` for the array, vector and member arms |
| 23 | `__aligned(1 << ENUM)` collapsed to 1 (alignment 256 → 8, `init_pwq` BUG) | enumerators are evaluated; "cannot evaluate" returns `None`, not the left operand |
| 24 | an 8 B `guard(mutex)` object stored the materialisation slot's address, so the destructor unlocked the wrong address | all three declaration sites ask `assign_needs_whole_copy` |
| 25 | a chained designator into a bitfield lost the bit offset (every IDT gate had `p = 0`) | `designator_bitfield` walks the whole chain; static and runtime paths share it |
| 26 | frame slots were a monotone watermark: completed block scopes and inlined copies kept their slots (`perf_event_task_tick` 11 904 → 1 264 B) | scope-aware `ScopedVarAlloc` releases the watermark at block exit |
| 27 | `"m"` asm operands carried a materialised value instead of an address; MMIO wrote into a frame slot | `constraint_prefers_memory`; `"m"` lowers to an address and rebuilds as `Deref` |
| 28 | `op $imm, sym(%rip)` relocated off by the immediate size, and 8-bit ALU used the 32-bit form | `push_rip_data_rel(…, tail)`; `0x80 /digit` for 8-bit |
| 29 | the zero-init of the frame wiped the over-aligned frame anchor, so the epilogue restored `rsp = 0` | emit the anchor after the zero init |
| 30 | `_Bool` conversion truncated to 8 bits (`0x200` → false), breaking `irqd_is_activated` | `emit_normalize_bool` + `emit_xmm0_to_bool`, and the same rule in constant folding |
| 31 | `++`/`--` zero-extended the stack slot and then added 64 bits (`-1` → `0xffffffff`) | read through `mov_from_stack` (declared width and signedness) |
| 32 | `call *%[paravirt_opptr]` lost GAS's 6-byte `ff 15 disp32` shape; the byte-matching alternative rejected it | `asm_static_sym_addr` → `SymDisp`; REX only when a register needs it |
| 33 | `security_hook_list` elements were 64 B apart while the consumer used 24 B (array-member address initialiser fell into a zero-length fallback) | `Member{ty: Array}` evaluates to `ConstValue::Addr`; fallbacks use the target slot width |
| 34 | compound-literal array elements were flattened by brace elision (`static_calls_table` mis-placed) | `is_braced_init` (`InitList \| CompoundLit`) at every "has braces" site |
| 35 | an 8 B aggregate statement expression was stored by slot address into a member (`nodemask_of_node`) | member and element initialisers ask `assign_needs_whole_copy` |
| 36 | a `for`-init cleanup scope closed inside the init clause, so `scoped_guard` destructed before the loop held the lock | `Stmt::For` closes the cleanup scope at the loop exit |
| 37 | a GNU range designator over an aggregate wrote zeros, so every `hlist_nulls` sentinel became `NULL` | range elements go through `encode_array_elem`; `[f ... l].field` is expanded first |
| 38 | `PreservedDecl` initialisers read homes by base name while SSA liveness only saw instruction uses; two parameters shared a register | liveness expands base-name readers to every version of that base |
| 39 | a speculatively inlined 672 B local pushed `do_one_initcall`'s frame to 1 792 B; four frame-economy defects surfaced while measuring it | inline gate on the largest local (256 B), direct binding of read-only parameters, dead-declaration pruning, fix of the double-counted variable band, static `"m"` printed as `sym(%rip)` |
| 40 | x87 inline asm had no RIP-relative form, no REX.B on memory operands, and one ESC table per mnemonic family (`fpu/bugs.c` did not build) | encoder rewritten from GAS's table, operand-kind and size aware |
| 41 | §5.39's "a read-only parameter may bind straight to the caller's lvalue" did not look at `asm`: `__ffs(word)`'s `"=r"(word)` output wrote through to the caller's `start`, so `__free_pages_memory` (mm/memblock.c) re-freed the same page forever and the boot hung right after `mem auto-init` | the direct-bind gate also asks `asm_written_roots_of` — asm/asm-goto outputs and memory-constrained inputs; the expression-inline path refuses the same shapes through `needs_slot` |

### 5.42 Callee-saved save area is demand-driven: `hdr = 16 + 8·cs + 8·temps`

The frame header used to reserve four callee-saved slots (`rbx`, `r12`, `r14`, `r15`) plus
8 B for every function, `hdr = 48 + 8·temps`. All four save/restore pairs were emitted
unconditionally and the unused ones were later overwritten with `nop`, so the *frame* paid
32 B even in the typical kernel function — the trace that motivated this had 119 frames
holding 1.11 pool registers on average.

Two things had to be settled before the header could shrink:

* **There was no 8 B hole.** The old `48` is `16 + 4·8`, and `16` is the canary cell plus the
  variable area's first slot (an object's top slot sits at `-hdr`, so the header's last two
  `8`s are two different things). The general form is `16 + 8·cs + 8·temps`; at `cs = 4` it is
  byte-identical to the old arithmetic, which is what
  `resolve_reproduces_the_legacy_offsets_bit_for_bit` pins. Nothing is reclaimed by
  re-deriving offsets — every byte saved comes from not saving registers the function does not
  use.
* **The count has to be static.** Prologue `sub`, the variable-area top and every temp-slot
  coordinate are emitted before the body is generated, so "count after the fact" is not
  available. `Codegen::compute_callee_plan` fixes the bitmap up front from (1) the final home
  table (file-scope `register asm`, parameter homes, backend-chosen homes), (2) every local
  declaration in the body — the allocator's `home` and the user's `register_name`, collected
  recursively — and (3) *any* `asm`: if the function contains inline assembly all four pool
  registers are planned, because `q`/`Q`/`r` operands plus a clobber list can consume the whole
  pool at emission time and the alternative failure is `E0311`/a clobbered caller.

`RegBank::set_plan_mask` makes the bitmap authoritative at emission: `alloc_temp`,
`alloc_temp_except`, `alloc_temp_named`, `is_free` and the asm scratch pool all skip registers
outside it, and `claim()` records `plan_violation` when a home reaches an unplanned pool
register. `patch_callee_saves` turns that into a hard `E1137` instead of silently nop'ing a
save whose slot does not exist (frame size and variable top are already emitted by then, so
growing the plan at that point is not possible).

Measured by rebuilding two scheduler TUs with the exact `.cmd` line kbuild used
(`scripts/frame_cs_plan_ab.sh <build-tree> kernel/sched/fair.o …`; frame = prologue pushes × 8
+ the first `sub rsp`). The control is the **same binary** with `DCC_CS_PLAN_ALL=1`, which
`compute_callee_plan` answers with the pre-change `0x0F`; because `cs = 4` reproduces the old
`48 + 8·temps` byte for byte, this is a clean A/B and not a comparison of two binaries:

| object | frames, 4-slot plan | on-demand | delta | improved / worse | p50 |
|---|---|---|---|---|---|
| `kernel/sched/fair.o` (229) | 40 520 B | 36 264 B | −4 256 B (−10.5 %) | 139 / 0 | 136 → 120 B |
| `kernel/sched/build_policy.o` (346) | 62 016 B | 56 496 B | −5 520 B (−8.9 %) | 182 / 0 | 136 → 120 B |

p75 on `fair.o` moves 200 → 184 B; on `build_policy.o` it stays at 232 B (the big functions
are the ones that really hold four registers). The gain is smaller than `(4 − cs)·8` per
function would suggest because 16-byte alignment swallows part of it, and it is concentrated
in functions that hold none or one pool register: the scheduler's hot frames are unchanged
(`pick_next_task_fair` 408 B, `task_tick_fair` 408 B, `dequeue_entity` 424 B,
`enqueue_task_fair` 456 B — plan `0x0F`, they do keep all four).

Defect this change exposed (fixed here, case in the suite): `collect_stmt_decls` never walked
`Stmt::For`'s `init` clause, where the parser stores the declaration of `for (int i = …)` (a
synthesized block). A loop variable homed in `r12` was therefore outside the plan and the new
fail-closed check fired `E1137` on six `cold_outline_*` fixtures and
`computed_goto_in_main_survives_the_builtin_linker_path` — i.e. the check found a real home
that the plan was missing, not a spurious one.

### 5.43 Three full-suite regressions the frame work surfaced

| # | Defect | Fix |
|---|---|---|
| 42 | `-O2` dropped `asm("nop")`: `InstKind::InlineAsm { volatile }` copied the AST flag, while both `inst_is_side_effecting` and `inst_has_side_effect` delete an asm that is neither `volatile` nor has outputs. GCC's rule is that an asm with no outputs is implicitly volatile | the IR flag is `*volatile \|\| outputs.is_empty()`; the AST field stays untouched because it separately controls the `mfence` sequence at emission |
| 43 | the `-S` text for a spilled left operand read `mov %rax, %-16(%r13)` (a stray `%` before the displacement), so GAS rejected it while the byte path was correct | the gas string is `format!("mov {}, {}", …)`; text and bytes come from the same site |
| 44 | `__kasan_rz_<var>_<j>` was placed at `off − slots·8 − 8j` while `off` is already the object's lowest slot, so for a multi-slot local the last redzone landed on the *next* variable (`struct S s; char b[8]` → `__kasan_rz_s_3` and `b` both at −120). The poison emitter's live-range check hid it, which is why it only showed up as a test that passed for the wrong reason | redzone j sits at `off − 8 − 8j`; the live-range guard stays as defence in depth. The case's `b[7] != 0` assertion was reading uninitialized stack (x86-64 does not zero frames; after the layout change that byte was `0xff`), so `b` is now initialized explicitly — the assertion still proves `memset(p+5,'z',2)` writes exactly two bytes |

### 5.44 Address-scratch pool exhaustion: borrow a live register instead of reusing `%r12`

`alloc_asm_mem_scratch` hands out one of four callee-saved registers (`rbx`, `r12`,
`r14`, `r15`) as the base of an inline-asm memory operand; the template expands the
operand as `(%reg)`, so the register must hold the address for the whole asm. When all
four were already holding live values the old fallback was `return 12`: `%r12` was
handed out **without saving it**, and the binding step overwrote whatever lived there.

kbuild22 died on exactly that (row in §4.3). `block/blk-mq.o::__blk_mq_alloc_disk`
ended in `… lock bts %rcx,(%r12); … mov %r12,%rax` — the function returned
`disk + 0x160`, because `r12` had been loaded with `disk + 0x160` for the
`set_bit(GD_OWNS_QUEUE, &disk->state)` operand. The gdb session on the stub pinned the
two sides of the mismatch: `__alloc_disk_node` stored the queue at
`disk=0xffff888000b96400 +0x50`, while `add_disk_fwnode` was entered with
`disk=0xffff888000b96560` (exactly +0x160) and `queue=(nil)`; reading
`disk->queue->mq_ops` then faults at address `0x10`. The chain is
`loop_init → loop_add → device_add_disk → add_disk_fwnode+0x6c`.

A temporary instrumented build printed the allocator state at that asm:
`alloc=0b1111 used=0b1111 plan=0b1111 owned=[3,4,8,12,14,15] banned=[] clob={}` —
the whole pool was allocated, nothing could be handed out, and the `12` fallback was
taken. The defect is pre-existing (the `return 12` predates the frame work), but it is
only reachable when a function keeps four pool registers live *and* contains a memory
operand, which is why it took a kbuild-scale corpus to surface.

The fix keeps one invariant: an operand address is never silently relocated.

* the first pass is unchanged — a free pool register via `alloc_temp_named` returns
  `(reg, borrowed = false)`;
* if every candidate is taken, one that is `is_allocated && !is_banned` is **borrowed**:
  `sub rsp,8; push reg` before the asm (`16 B` total, so the call-site alignment
  invariant is untouched), and `release_asm_mem_scratch` pops borrowed registers in
  reverse order before freeing the ordinary ones;
* if no register is borrowable (all excluded by clobbers/fixed constraints) the
  statement is rejected with `E0311` instead of emitting a wrong operand;
* `asm goto` cannot use a borrowed register — its jump-out path skips the `pop`, which
  would both lose the value and shift `%rsp` permanently — so borrowed registers join
  the unguardable set (`E0317`), same criterion as a clobbered live register.

Rebuilding the same TU with the fixed compiler gives
`sub $0x8,%rsp; push %rbx; mov -0x68(%r13),%rbx; … lock bts %rcx,(%rbx); pop %rbx;
add $0x8,%rsp; mov %r12,%rax`: `%r12` keeps `disk`, `%rbx` (which held a spilled value)
is saved and restored, and the return value is `disk` again.

Regression cases: `asm_mem_scratch_borrows_when_pool_exhausted` — four file-scope
`register asm` globals keep the pool busy, so the `-O2` object must contain the
`sub $0x8,%rsp; push %rX; … lock incq (%rX); pop %rX; add $0x8,%rsp` shape, and all
four `-O` levels must match gcc at runtime; `asm_mem_scratch_pool_exhaustion_fails_closed`
— the same busy pool plus all four registers in the clobber list must die with `E0311`
and produce no object.

### 5.45 A braced string literal neither sized nor filled a character array

`static char core_modes[] = { "file\npipe" "\nsocket" };` (`fs/coredump.c`) is
C11 6.7.9p14 with the optional braces. Two separate places ignored the braces:

* dimension inference (`ast::deduce_incomplete_array_dim`) recognised a bare
  `Expr::String` initializer but not `InitList[String]`, so the array fell through to
  "one initializer item = one element" and was sized **1 byte**;
* the data emitter's array arm (`gen_global_init_list`) handed the string to
  `encode_array_elem`, which recognises a whole-row string only when the *element* is
  a character array (the 2D-array case). For a scalar `char` element the string
  degraded to its first byte.

Reproduced in four lines: `static char a[] = { "hello" };` produced an object symbol
`a` of size 1, `sizeof(a) == 1`, and data bytes `68 00 00 00 00 00`. In the kernel this
shows up twice — `sizeof(core_modes) - 1` becomes 0, so `register_sysctl_sz` rejects
the whole table (`sysctl table check failed: kernel/core_modes No maxlen`, kbuild22
log), and the 17-byte initializer is written into a 1-byte object, stomping whichever
static the linker placed after it.

Fix: both sites treat a one-item braced string as the string rule — dimension =
`data.len()` (divided by the element width for wide strings), data = the full bytes
spanning consecutive elements. Pointers (`char *p[] = { "x" }`) keep the one-item
rule. Regression case `braced_string_literal_array_dim` covers file-scope, `const`,
function-local `static`, a declared-larger buffer, a `.maxlen` table shaped like the
kernel's, and all four `-O` levels against gcc.

### 5.46 An array compound literal on a pointer target was stored by value, not by address

`drivers/ata/libata-eh.c` builds its timeout table with

```c
#define CMDS(cmds...) (const u8 []){ cmds, 0 }
static const struct ata_eh_cmd_timeout_ent
ata_eh_cmd_timeout_table[ATA_EH_CMD_TIMEOUT_TABLE_SIZE] = {
	{ .commands = CMDS(ATA_CMD_ID_ATA, ATA_CMD_ID_ATAPI), .timeouts = … },
	…
};
```

C11 6.3.2.1p3: the compound literal has array type, so the initializer is its
**address** (array-to-pointer conversion). dcc's struct-field arm of
`gen_global_init_list` matched every `InitList`/`CompoundLit` value and recursed as if it
were a nested initializer list, so the array's *contents* were written into the 8-byte
pointer slot — with **no relocation at all**. In the kbuild23 tree the object showed:

```
.data  0050 eca10000 00000000  00000000 00000000   ← .commands = 0xa1ec
       0060 2f470000 00000000  …                  ← .commands = 0x472f
.rodata (no CMDS arrays: nothing referenced them)
```

`0xa1ec` is literally `{0xec, 0xa1}` read as a little-endian integer, and the fast path is
the first boot-time caller: kbuild23's `--boot` died with
`BUG: unable to handle page fault for address: 000000000000a1ec`,
`RIP: ata_lookup_timeout_table+0x62`, `RAX=0xa1ec`, `Comm: scsi_eh_1` right after the
libata probe — exactly the `mov (%rax),%rax; movzbl (%rax)` in its Code dump, i.e. the
first dereference of the `commands` pointer. (The `timeouts` field was correct because
it names an array object, and name references already went through the relocation path.)

Fix, one predicate and three call-site guards:

* `Codegen::ptr_decay_literal(target, value)` is true only for a **pointer target** and an
  `InitList`/`CompoundLit` whose own type is an **array** (`Expr::ty()`); struct/union
  literals do not decay (`struct S *p = (struct S){…}` is a constraint violation — gcc:
  "incompatible types" — the address must be written `&(struct S){…}`).
* `Codegen::ptr_decay_init` materialises the anonymous array through the existing
  `materialize_compound_literal_rel` path (`.rodata` + `.L.compound.N` local symbol) and
  emits one `R_X86_64_64` at the pointer slot, routing `.rodata`-internal slots through
  `push_data_symbol_rel`; ET_EXEC keeps the absolute-address form.
* `eval_const_expr` asks it first (so every caller that reaches the constant evaluator is
  covered), and the three sites that unwrap a brace list *before* the evaluator —
  `gen_global_init_list`'s struct-field and array-element arms, `eval_global_init`, the two
  custom-section arms and the regular `.data` arm in `compile_rel`, plus
  `register_static_global` — now skip the nested-list recursion when the target is a
  pointer.

The rebuilt object now carries what gcc emits:

```
.data  0050 R_X86_64_64  .L.compound.0      .data  0058 R_X86_64_64  ata_eh_identify_timeouts
.rodata 0000 eca100 2f4700 f82700 f93700 ef00 9100 e7ea00 4000
```

Regression case `ptr_decay_of_array_compound_literal_emits_relocation`
(`tests/fixtures/c/ptr_decay_compound_array.c`) reproduces the kernel's `CMDS()` shape as a
struct field, a top-level pointer and a pointer-array element, runs the four `-O` levels
against gcc, and additionally asserts `objdump -r` shows at least four `.L.compound.*`
relocations in `.data` — a run-only test can pass by luck when the raw bytes happen to be
small, but the missing relocation is the defect.

### 5.47 An assignment used as a subscript scaled the lvalue's address, not the assigned value

`sound/core/seq/seq_clientmgr.c` (the second Oops of the kbuild23 run, right after §5.46):

```c
clienttab[client->number = c] = client;
```

C11 6.5.16p3: the value of an assignment expression is the stored value converted to the
lvalue type. The subscript consumer asks for the index in `%rcx` (`gen_expr(idx, 1)`), but
the memory-lvalue arms of `Expr::Assign` computed the RHS into `%rax`, stored it through the
address in `%rcx`, and left the value in `%rax` — only the Deref arm had a `dst == 1`
workaround (added earlier for shift counts). The index path then scaled `%rcx`, i.e. the
lvalue **address**:

```
mov %rax,%rcx        ; rcx = &client->number
movslq %esi,%rax     ; c
mov %eax,(%rcx)      ; store
imul $0x8,%rcx,%rcx  ; scales &client->number, not c
```

kbuild23 hit it at `seq_create_client1+0x78f` as `general protection fault … non-canonical
address 0xfffc43ff90eab168`; the minimal repro `g[s->n = c] = 42` segfaults at `-O0`/`-O1`
(at `-O2` the value happened to stay live in a temporary, which is why only the kernel —
whose SSA lowering kept the shape — exposed it in the field).

Fix: `Codegen::deliver_scalar_assign_value(dst, l_ty)` — when the caller asked for a
specific register, `mov dst, rax` after the store (the value was already reduced to the
lvalue type by `convert_assign_rhs_in_reg(0, …)` before the store). It is called from every
scalar memory-lvalue arm: Index (semantic materialise, Ident fallback, general fallback),
Deref (replacing the `dst == 1` special case), Member non-bitfield, and the generic
Cast-lvalue fallback. Bitfields already delivered through `%rdx`.

Writing the regression case surfaced a **second, independent** defect:
`g[(s.n = 4) + 1] = 500;` stored `g[5]` but never assigned `s.n`. `compute_index_offset`
(`semantics.rs`) folded the subscript with `const_eval::eval`, whose `Expr::Assign` arm
deliberately returns the RHS value and leaves the store "to runtime"; the constant then
**replaced** the whole subscript expression. The fold is now gated by the same predicate
the optimiser uses before replacing an expression
(`discarded_value_needs_code && !dropping_value_is_safe`), so only structurally
side-effect-free subscripts become `ByteOffset::Constant`.

Regression case `assign_expression_as_index_delivers_value_and_keeps_side_effect`
(`tests/fixtures/c/assign_as_index_value.c`): `p->f`, `s.f`, `a[i]` and `*p` as subscripts,
the kernel's pointer-array shape, assignment-value reduction (`uchar = 300 → 44`), a nested
`(s.n = 4) + 1` subscript and a shift count, all four `-O` levels against gcc.

### 5.48 A statement expression did not open a scope, so a shadowing `_Bool` re-typed a pointer

`this_cpu_xchg` expands to `__pcpu_size_call_return2`, which declares its result as
`TYPEOF_UNQUAL(variable) pscr2_ret__` and then runs `do { … } while (!({ … }))`, where the
condition is a second statement expression declaring `bool pscr2_ret__` (`__this_cpu_try_cmpxchg`).
`TYPEOF_UNQUAL` resolves to `__typeof__` in this configuration — the compiler reports
`__clang_major__ 18`, so `CC_HAS_TYPEOF_UNQUAL` is 0 and the macro takes the `__typeof__` branch
(preprocessed evidence: `kernel/fork.i:110487`) — so the outer temporary is a pointer, the inner
one a `_Bool`, and they share a name.

`parser.rs` parsed `({` as `(` followed by a block **without pushing a scope**, so the inner
declaration landed in the scope that already held the outer one. `rename_shadow_var` renames only
when `exists_in_outer_scopes(name)` says so, which cannot see an object declared in the *same*
scope, so no rename happened. The backend's `var_map` is keyed by name alone and `register_var`
keeps the slot while overwriting the type, so `_Bool` won: every store to the outer pointer ran
through the `_Bool` normalisation (`test`/`setne`/`movzbq`) and the value collapsed to 0/1.

In the field, `alloc_thread_stack_node_from_cache`'s `if (vm_area) return vm_area;` returned `1`
instead of the pointer and `alloc_thread_stack_node` dereferenced it: `CR2=0x2d` (`1 + 0x2c`).

Fix: the statement-expression block is parsed between `push_scope()` and `pop_scope()`. The
26-line shape of the same expansion returned `1` instead of `0x1234` on the unfixed compiler.
Regression case `statement_expression_has_its_own_scope` (`-O0`…`-O3`).

### 5.49 Expression-level inlining leaked the argument type into the parameter

`substitute_call` (`opt.rs`) clones each argument into the callee body by name; the parameter
declaration is gone by then, so the clone keeps the **caller's** type. For

```c
static inline struct request *blk_mq_rq_from_pdu(void *pdu)
{ return pdu - sizeof(struct request); }
static inline struct request *scsi_cmd_to_rq(struct scsi_cmnd *cmd)
{ return blk_mq_rq_from_pdu(cmd); }
```

`pdu - 0xf8` was scaled by `sizeof(struct scsi_cmnd)` (0x128) instead of by 1, i.e.
`cmd + 0xfffffffffffee140`. `__ata_scsi_queuecmd` then read `rq->tag` through that address,
`__ata_qc_from_tag()` returned NULL, and `qc->tag = qc->hw_tag = tag` wrote `0x5c` — the kbuild24
Oops (`CR2=0x5c`).

Fix: every argument is converted to the declared parameter type (C11 6.5.2.2p7) before
substitution. Statement-level inlining was already correct: it materialises a temp with `p.ty`.
Regression case `inline_param_uses_declared_param_type` (`-O0`…`-O3`): `p - 1` and `p + 1` on a
`void *` parameter called with `struct big *` (sizeof 0x40), plus the pointer difference.

### 5.50 The inlined result kept the body's type, not the declared return type

Second half of the same site. `scsi_cmd_to_rq()` is declared to return `struct request *` and its
body returns a `void *` expression. Substituting the body made the call expression `void *`, so
`scsi_cmd_to_rq(cmd)->tag` had no member table to resolve against and the field was emitted at
offset 0 — `list_head.next` read as the tag. `__ata_qc_from_tag()` indexed `ap->qcmd[]` with that
value and returned NULL, and the same `qc->tag = qc->hw_tag = tag` wrote `0x5c` again (this is
how the image carrying §5.48–§5.49 faulted).

Layout was ruled out with a probe compiled from libata-scsi's own `.cmd` line: `sizeof(struct
request)` 0xf8, `sizeof(struct scsi_cmnd)` 0x128 and `offsetof(struct request, tag)` 0x20 are
identical under dcc and gcc.

Fix: the substituted return expression is converted to the function's declared return type
(C11 6.8.6.4p3). Regression case `inline_result_uses_declared_return_type` (`-O0`…`-O3`), whose
non-inlined control is a `noinline` function of the same shape.

### 5.51 A bitfield initialiser list still rewrote through `-24`/`-32`/`-40(%r13)`

The second defect the §5.42 header change exposed, found by the pre-commit torture gate
(seven `execute/` failures, all `-O0`-only: `bf64-1.c`, `bitfld-5.c`, `pr110166-1.c`,
`pr39339.c`, `pr78170.c`, `pr98366.c`, `20040307-1.c`). `aggregate.rs::gen_init_list_at`'s
bitfield arm computes container address / old container / new value in three stack slots and
they were written as literals `-24`, `-32`, `-40`.

Those three cells are the callee-saved save area (`cs` slot *j* sits at `-(16+8·j)`, so
`j = 1,2,3`). Before §5.42 all four slots were reserved unconditionally, so the path was
harmless only when the function never actually held a pool register — the save/restore pair
was `nop`-patched and the slot nobody read back. With `hdr = 16 + 8·cs + 8·temps` a function
that holds no pool register has a 32 B header, so the same literals land **inside the
variable area**: in `struct tmp a = {0x123, 0xFFF000FFF000F}` the value slot `-0x28` is the
next struct local `b` and the address/container slots `-0x18`/`-0x20` are `a` itself. Two
structs initialised in one function overwrote each other; the probe printed
`raw a=1230000000000123 b=1230000000000123` where gcc prints
`raw a=fff000fff000f123 b=123fff000fff000f` (each struct ending up with the *other* member's
value in both fields).

Fix: the three slots are the bitfield RMW band's layer geometry
(`frame_layout.bf_rmw_slot(depth, 0/1/2)`, 3×8 B per 32 B layer, slot 0 = address,
1 = container, 2 = new value, as in `increment.rs`), and the depth counter is incremented
around the value expression so a nested RMW inside it takes the next layer. That makes the
reservation mandatory, so the static scan learns the site: `bf_rmw_chain_depth` treats a
`CompoundLit` whose type contains a bitfield as a site, and `bf_rmw_depth_item` does the same
for a `Decl::Var` whose initialiser is an `InitList` and whose **declared** type contains one
(the literal `InitList` carries the placeholder `Type::Int` from `parser.rs`, so the question
has to be asked of the declaration). `type_contains_bitfield` recurses through nested
aggregates and array element types, and answers "yes" for an unresolved `Type::Named` —
over-reserving costs 32 B, under-reserving writes out of the band.

Regression case `bitfield_init_list_uses_rmw_band`
(`tests/fixtures/c/bitfield_init_list_slots.c`) checks both container words and both read
paths at `-O0`/`-O2`; `scripts/torture_test.sh all` is back to **0 failures**
(3 707 pass, 6 skip).

### 5.52 An inline temp's member access read an object address as its value

`__always_inline` expansion materialises the callee's parameter as a statement
``expression (`({ struct t rv; …; rv; })`). `gen_expr` delivers the **address** of that temp in
`rax` — the same convention as a compound literal — but three sites disagreed:

1. `cond_materializes_aggregate` carried a `value_is_whole_copy(ty)` size test, so an
   aggregate ternary of ≤16 B took the scalar path and each arm delivered per its own
   shape. `const vma_flags_t v = is_exec ? mk_vma_flags(VMA_EXEC_BIT) : EMPTY_VMA_FLAGS;`
   (8 B bitmap word) stored the temp's **stack address** into `v` and passed it by value to
   `do_brk_flags`: the anonymous BSS VMA's `vm_flags` became `0xffffc900…`, bit 3 fell out
   of the even address, `do_anonymous_page` returned `VM_FAULT_SIGBUS` for a `VM_SHARED`
   VMA, and the first user write to `__environ` killed init.
2. `gen_expr_member_rvalue`'s materialisation gate excluded only `CompoundLit`. The
   StmtExpr arm, memory-returning calls, aggregate ternaries and chained assignments all
   deliver an address too, so the gate re-stored the address bytes as the value and member
   reads came back with the stack pointer: `vma_flags_to_legacy(append_vma_flags(…)).__vma_flags[0]`
   produced `0xffffc900…`, GUP found no `VM_WRITE` and returned `-EFAULT`, exec reported
   `-E2BIG`.
3. `gen_lvalue_addr` had no arm for an aggregate rvalue used as a member/subscript base, so
   `(<stmt-expr>).w[0]` (the shape form-level inlining leaves behind — `substitute_call`
   rewrites `f.w[0]`'s `f` into the parameter's StmtExpr) fell back to the materialisation
   branch: the buffer address was stored into the result slot and the slot's **address** was
   used as the object address. The fixture read a stack pointer back (`0x7ffe…` instead of
   `0x77`).

Fix: the delivery-shape question has one truth source, `rvalue_is_buffer_addr`, and it
answers by **shape only** — `cond_materializes_aggregate` no longer asks about size. The
`gen_lvalue_addr` match grows a guard arm immediately before its `_ => false`:

```rust
_ if Self::rvalue_is_buffer_addr(e) => {
    self.gen_expr(e, dst);   // rax = address of the object
    true
},
```

which answers for **every** buffer-addr shape at once (`StmtExpr`, `VaArg`, an aggregate
`Assign` whose route whole-copies, a materialised aggregate `Cond`) instead of spelling
`StmtExpr` out a second time, and subsumes item 2: `gen_expr_member_rvalue`'s fallback is no
longer reached for those shapes. The `Call` arm above it is untouched — it still distinguishes
`>16 B` (sret buffer, `rax` is the address) from the register-returned `≤16 B` cases, which
the truth source reports consistently. Regression case
`agg_member_of_inline_temp` (`tests/fixtures/c/agg_member_of_inline_temp.c`) cross-checks
`-O0`…`-O3` against gcc: bits 1–16 (all five member paths — 8 B and 16 B structs, a nested
`.inner.b`, an array-field element through a member expression, and a compound-literal
argument) failed on stock dcc, while the outlined-call control (bit 32) passed both before
and after.

### 5.53 A large aggregate initialiser skipped the zero-fill above 128 B

C11 6.7.9p19/p21 requires every member not explicitly initialised (padding included) to be
zeroed. `gen_init_list` did that only when `struct_size <= 128`: bigger aggregates got **no
zero-fill at all**. `struct vma_merge_struct` is 0x90 B, and its `VMG_MMAP_STATE` designated
initialiser left stack garbage in `.target`; `vma_expand()` dereferenced the value 1 as a
`vm_area_struct *` (`#PF at 0x29`).

Fix: zero the whole object regardless of size — one `mov_imm(0)` followed by
`zero_mem(base, struct_size)`, whose 8/4/2/1 store sequence writes only `rax` and leaves
`rcx`/`rdi` (live during the initialiser expressions) alone. Regression case
`big_struct_designator_zero_matches_gcc` (`tests/fixtures/c/big_struct_designator_zero.c`):
144 B and 136 B structs, the 128 B boundary, a 160 B union and a nested large struct.

### 5.54 An anonymous struct definition polluted `typedef struct {}`

`include/net/net_trackers.h` has `typedef struct {} netdevice_tracker;`. An anonymous
struct/union definition has an empty tag name, and the parser registered its fields in
`struct_defs` under that empty key — while `resolve_type_latest` also *looked up* the empty
key for empty-typed typedefs. So the moment any later header wrote `struct { … } foo;` (or an
anonymous member), `netdevice_tracker` "grew" that struct's fields. The damage is not
local: the same type has a different `sizeof` in different TUs (0/2/4/24 observed), which
shifts every following member. `struct inet6_dev.refcnt` was at 0x280 in
`net/ipv6/addrconf.c` and 0x270 in `net/ipv6/mcast.c`, while gcc has 0x268 in both. At boot
`in6_dev_get` incremented whatever field sat at the offset in that TU — the
`refcount_t: addition on 0` + `saturated` warning pair — and the corrupted folio LRU list
surfaced later as a GPF in `exit_mmap → lru_activate` when init exited.

Fix: anonymous definitions are not entered into `struct_defs` (no tag to refer to them by),
and `resolve_type_latest` only consults the table for a non-empty name. Regression case
`empty_struct_typedef_layout_matches_gcc` (`tests/fixtures/c/empty_struct_typedef_layout.c`):
two empty typedefs around two anonymous definitions, sizes and member offsets, and a member
read that goes through `resolve_type_latest_deep` (failure bits 1/2/4/8/16/32/64).

### 5.55 A constant-switch fold left its nested `break` bound to the caller's loop

The constant-selector `switch` fold dropped the switch but only removed `break`s that were
at an arm's top level or closed an arm's block. A `break` nested inside `if`/bare block stayed
in the tree, and with the switch gone it bound to the nearest enclosing **loop**. The kernel
shape is `include/linux/rmap.h::__folio_try_dup_anon_rmap`:

```c
case PGTABLE_LEVEL_PTE:
    if (!folio_test_large(folio)) {
        if (PageAnonExclusive(page))
            ClearPageAnonExclusive(page);
        atomic_inc(&folio->_mapcount);
        break;              /* nested in the if, not at arm level */
    }
```

`__always_inline` expands this into `copy_pte_range`'s `do { … } while`, so the stray
`break` jumped out of the whole page-table copy loop: in `mm/memory.o` five
`lock incl (%rbx)` sites (`_mapcount`, offset 0x30 — the `kcsan_check_access` right before
it checks size 4) were followed by `jmp` to the loop exit (`_raw_spin_unlock(src_ptl)` /
`__rcu_read_unlock` / `add_mm_rss_vec`), skipping the rmap accounting and
`__copy_present_ptes`. Forked children ended up with unpopulated page tables.

Fix: after the fold, scan the emitted items (prelude + selector evaluation + kept arms) and
rewrite any remaining `break` that belongs to the folded switch into `goto __inlN__swend`,
appending the synthetic label only when something was rewritten
(`rewrite_breaks_to` recurses through `If`/bare block/labelled statement and does **not**
cross `while`/`do`/`for`/nested `switch`; statement expressions are not entered, same rule as
`rewrite_returns_to_dest`). The "emit the label only when a real writer rewrote something"
rule is the same one `rewrite_returns_to_dest` follows: a dead `Stmt::Label` makes
`ssa/structure.rs::stmt_has_unstructured_flow` push the whole function onto the
analysis-only path, which would re-break the constant switches this pass exists for. The
`is_inliner_label` documentation now covers the third synthetic namespace member
(`__inl<N>__swend`, alongside the inline exit and the prefix-renamed labels).

Evidence: in the rebuilt `mm/memory.o` the five jumps now target the inline block's tail
(`0x17a8d`/`0x19d10`/`0x1a1bf`/`0x1b7f3`/`0x1c250`, blocks that open with
`mov $0,%eax; mov %eax,-0x218; jmp …` — i.e. `return 0` into the `__inl$rv` exit), and the
old `jmp 0x1cd4c` (loop exit) count is zero. Reproducer
`tests/fixtures/c/switch_fold_nested_break.c` (always-inline helper, constant switch,
`break` inside `if`, caller `do { } while`): stock dcc returns 0 at `-O1`/`-O2`/`-O3`
(`t = 0` instead of 11) where gcc and the fixed dcc return 11; `-O0` was always correct
(the fold does not run there). Regression case
`const_switch_fold_nested_break_stays_in_switch`.

### 5.56 `-O2` left an expression-position `__always_inline` unexpanded: 16 KB stacks ran dry

The `-O2` inliner expands only `always_inline`/`flatten` callees. Even for those, the
expression-position route (`inline_forced_call_as_stmt_expr`, borrowing
`try_static_inline_general`) refuses arguments with side effects — a *general* static inline
would then evaluate an argument more than once. With statement-expression binding the
parameters are temporaries, so each argument is evaluated exactly once, left to right; the
retry that passes `allow_effectful_args = true` existed but was gated on `args_have_forced`
("some argument itself contains a forced call that this pass just expanded").

The kernel shape is `__update_load_avg_se`:

```c
if (___update_load_sum(now, &se->avg, !!se->on_rq, se_runnable(se), cfs_rq->curr == se))
```

`se_runnable` is a plain `static inline` whose body has an `if`, so expression-position
inlining does not fire either; the argument stays a `Call`, `expr_has_side_effects(Call) ==
true`, and the outer `__always_inline` was vetoed on every site. Measured on the scheduler
chain, dcc vs gcc frames: `__schedule` 528↔104, `pick_next_task_fair` 400↔128,
`set_next_entity` 320↔24, `__update_load_avg_se` 192↔32 B; `___update_load_sum` (288 B) and
`rmqueue_pcplist` (400 B) do not exist as symbols in the gcc `vmlinux` at all. Deeper chain ×
larger frames is what put the 16 KB task stack 8 bytes past its guard page in `kernel_init`'s
PCI/ACPI path (`decay_load+0xc`, RSP `ffffc9000000fff8` — the flaky kbuild21 crash of §4.3).

Fix: the retry condition is "the arguments have side effects" (before or after the recursive
descent), not "the arguments contain a forced call":

```rust
if args_have_forced || args_dirty_before || args_dirty_after { … allow_effectful_args = true … }
```

Non-forced callees remain blocked by the `forced` gate inside
`inline_forced_call_as_stmt_expr`, so this cannot migrate a plain `static inline` into
expression position. Evidence: rebuilding `kernel/sched/build_policy.o` with the exact kbuild
`.cmd` line drops `___update_load_sum` from the symbol table entirely (no `nm` definition, no
call relocation in `objdump -dr`), matching gcc's shape.

### 5.57 A `__aligned__` argument carried a GNU `c ?: b` and the parser lost a `)`

`include/linux/cache.h` spells `__cacheline_group_begin_aligned(GROUP, ...)` as
`__aligned((__VA_ARGS__ + 0) ? : SMP_CACHE_BYTES)`. Three parser gaps combined:

1. `skip_attributes_extract_aligned` entered the attribute's argument `(` without
   incrementing its bracket counter, so the attribute's own two `)` were consumed one short:
   the residual `)` stayed in the token stream and `include/net/netns/ipv4.h:93/96` died with
   `E0003 expected type, got RParen` while compiling `init/main.c`;
2. `parse_aligned_expr` had no `?` branch, so the GNU omitted-middle `?:` (and the ordinary
   `c ? a : b`) stopped the constant fold — the same residual `)`;
3. `parse_aligned_atom` rejected unary `+` (the `( + 0)` spelling an empty `__VA_ARGS__`
   produces) and folded a literal `0` to `1`. Both are wrong inside a sub-expression:
   `(0) ? 32 : 64` must be 64 and `(8 + 0) ? : 64` must be 8.

Fix: depth accounting on the argument parens; a `?`/`?:` branch (when the condition is not a
known constant, the result is constant only if both arms agree); unary `+`; and the literal is
kept as written. Probes `__aligned__((8) ? 32 : 64)` → 32, `((0) ? 32 : 64)` → 64,
`((8) ?: 64)` → 8, `((0) ?: 64)` → 64, `((8 + 0) ?: 64)` → 8; two cacheline-aligned structs
then measure 0xC0.

### 5.58 `bswap r32` into a high register swapped the wrong register: ext4 could not mount

`bswap r32/r64` carries its register in the opcode's low three bits plus REX.B; there is no modrm.
`Insn::bswap_reg` emitted REX only for the `B64` width, so a 32-bit swap whose destination was
`%r8`–`%r15` was encoded `0F C8` — `bswap %eax` — instead of `41 0F C8` (`bswap %r8d`): the value
in the high register was left untouched and `%eax`, someone else's live value, was swapped instead.

The kernel shape is `fs/jbd2/journal.c:journal_check_superblock`:

```c
if (sb->s_header.h_magic != cpu_to_be32(JBD2_MAGIC_NUMBER) || …
```

On x86 `cpu_to_be32` is `__swab32`, which for a compile-time constant folds through
`__builtin_constant_p(…) ? __builtin_bswap32(__x) : __swab32p(&__x)`. The cast-wrapped argument
`(unsigned)(0xc03b3998U)` bypasses the parser's literal fold (that block only takes a bare
`Number`), so the swap is emitted by `gen_builtin`. The left operand is live in `%rax` and the
right one lands in the first callee-saved stage register, `%r8`:

```
lea SB(%rip),%rax ; mov (%rax),%eax      # h_magic, LE 0x98393bc0
mov %rax,-16(%r13)                       # park the left operand
movabs $0xc03b3998,%r8
bswap %eax                               # meant to be bswap %r8d
mov -16(%r13),%rax
cmp %r8d,%eax                            # 0x98393bc0 vs 0xc03b3998: never equal
```

Every ext4 mount therefore ended with `JBD2: no valid journal superblock found` /
`EXT4-fs (vda): Could not load journal inode`, and the runtime panel stopped at
`RT|ext4|FAIL:mount`.

Fix: emit REX.B for the B32 width too, and attach `gas_text` so `-S` renders `bswap %r8d` instead
of `/* raw */`:

```rust
match width {
    Width::B64 => { let mut rex = 0x48; if reg_idx >= 8 { rex |= 0x01; } out.push(rex); },
    _          => { if reg_idx >= 8 { out.push(0x41); } },
}
```

Repro, now fixture `tests/fixtures/c/bswap_high_reg_rex_b.c` (bits 1–32; bit 64 is the literal
fold below):

```c
static unsigned g = 0x98393bc0u;
int main(void) { return g != (unsigned)__builtin_bswap32((unsigned)(0xc03b3998U)); }
```

dcc returned 1 and gcc 0 at every `-O` level; after the fix both return 0 and the swap is `41 0F C8`.
The B64 and `bswap16` (`rol r16,8`) paths in the same fixture show the same REX.B requirement.

The same audit turned up the literal fold behind the `E0006` seen while bisecting:
`const_eval::eval_int_lit` stripped the `u/U/l/L` suffix **only in the decimal branch**, so
`eval_int_lit("0x11223344u")` returned `None`; the parser's `__builtin_bswap*` and
`__builtin_abs` arms had already consumed the `Number` and the `)`, leaving the `;` to be parsed as
an expression (`__builtin_bswap32(0x11223344u) == …`). Suffixes are now stripped before the radix
dispatch, and both fold arms decide **before** consuming tokens, so an unfoldable literal falls back
to the ordinary call path (fixture bit 64, unit test
`const_eval::tests::eval_int_lit_strips_suffix_in_every_radix`).

Evidence: with 46 TUs (`fs/jbd2`, `fs/ext4`) rebuilt by the fixed compiler the same image boots to
`RT|ext4|magic roundtrip umount` and `RT|exit|0`, and all 21 `RT|`/`MOD|` lines of that panel
are byte-identical to the gcc-built kernel (`scripts/runtime_closure.sh --compare`: PASS, 21/21).
The panel has since grown to 26 lines with the stress layer (§4.3); the 26-line comparison is
the current judge.

### 5.59 A runtime `offsetof` subscript swallowed the member segment that followed it

`fib_trie` insert crashed with a GPF in `put_child`: the faulting `chi` was not a node at all
(`ffffea0000273540`, vmemmap junk), and the kernel's own `TNODE_SIZE(1ul << bits)` was the
reason. `offsetof(struct tnode, kv[0].tnode[n])` evaluated to **32** — the offset of `kv`
alone — instead of `40 + 8n`.

Two code paths disagreed and both were wrong, but only for the shape "subscript followed by
another member":

* `compute_offset` handled `kv[0]`, then tried to descend into the **field's** type
  (`struct key_vector[1]`) instead of the element type, hit `Type::Array`, and stopped at
  `offsetof(kv)`.
* `find_offsetof_field_type` ignored subscripts entirely, so the runtime path scaled the first
  subscript with the array type and dropped every later segment.

dcc `-O2` produced exactly that: `mov $32,%eax; mov $0,%ecx; shlq $4,%rcx; addq %rcx,%rax`
(only `kv[0]`, stride 16). `tnode_new()` therefore `kzalloc(32)`; `tn = tnode->kv` is at
`+0x20`, so the `key_vector` header and the child slots were written past the object into the
neighbouring slab. Minimal repro (`TNODE_SIZE(2)` / `(4)` / `(1ul << 1)`): gcc 56/72/56 at every
`-O`; dcc 32/32/32 before, 56/72/56 after.

Fix: one plan builder, `offsetof_plan()`, used by both codegen entry points. Constant
subscripts fold into the base offset; runtime subscripts carry their own element type so each
stride is `sizeof(element)`, and the member walk continues from the type **after** the
subscripts — which is what `kv[0].tnode[n]` needs to reach `tnode`.

The same blind spot had a second instance one layer up: the SSA builder lowered
`Expr::Offsetof` to a literal with `eval_offsetof(ty, member)`, i.e.
`eval_offsetof_opt(…).unwrap_or(0)`, so at `-O2` a function containing a runtime subscript
evaluated to **0** while `-O0` (AST codegen) was correct. The fixture caught it
(`v2 = v4 = sh1 = sh2 = 0` at `-O2`). The SSA builder now calls
`unsupported("offsetof with runtime subscript")`, which keeps that function on the AST channel
where `offsetof_plan` runs; correctness beats the lost SSA pass.

Evidence: the rebuilt object reports `TNEW0 … size=56` (bits=1) and `size=72` (bits=2), every
child slot starts `NULL`, `RT|net|eth0 arp-complete` and `RT|exit|0` are reached, and the
`--stress` round is byte-identical to gcc (26 lines, §4.3). Regression case:
`tests/fixtures/c/offsetof_runtime_subscript.c`
(`c_offsetof_runtime_subscript_gcc_cross_check`, gcc cross-check at `-O0/-O1/-O2/-O3`,
exit codes 1–6 name the failing shape).

### 5.60 Two nested `scoped_guard()` declarations shared one frame slot

`scoped_guard(mutex, &mm->mm_cid.mutex)` expands to
`for (CLASS(mutex, scope)(&mm->mm_cid.mutex), *done = NULL; !done; …)`; two nested guards both
declare `scope` in their for-init clauses. Ordinary block declarations are renamed by
`rename_shadow_var` when they shadow an outer name, but the for-init path called `parse_decl`
directly: the declaration had already been inserted into `locals` before any check, and the
check was never run. The backend's `var_map` is keyed by name, so both guards got one slot; the
inner `raw_spinlock_irq` guard overwrote the outer mutex guard, the outer cleanup released the
spinlock as if it were the mutex, and `mm->mm_cid.mutex` stayed locked — `pthread_create` and
thread exit blocked in `sched_mm_cid_fork` / `sched_mm_cid_exit` with every CPU idle.

Fix: snapshot the visible names (`locals.all_keys()`) **before** parsing the for-init
declaration, rename a declarator that shadows one of them (or a global), and apply the resulting
map to the remaining init declarations, the condition, the step and the body — the C for-init
scope. `rename_shadow_var_if` now holds the common rename and skips `extern` declarations, which
name a global rather than a new object.

Evidence: `RT|sched|8x20000=160000` and `RT|threads|4x20000=80000` are part of the 26-line
stress panel and byte-identical to gcc (§4.3). Regression case
`tests/fixtures/c/nested_guard_slots_distinct.c`
(`nested_guard_same_name_slots_distinct_matches_gcc`, `-O0`…`-O3`, exit codes 1/2/4/8 name the
failing shape).

## 6. Reproduce

```bash
DCC=/root/dcc/target/release/dcc
TREE=/root/linux-pvh-src
BUILD=/root/pvh-lmc3

# 0. dcc itself
cd /root/dcc && cargo build --release -j2

# 1. configuration: localmodconfig + PVH, no 16/32-bit C paths
KERNEL_TREE=$TREE BUILD=$BUILD DCC_BIN=$DCC \
  bash scripts/kernel_pvh_verify.sh --config

# 2. full vmlinux build with gcc traps armed (expect: traps 0, delegates 0)
KERNEL_TREE=$TREE BUILD=$BUILD DCC_BIN=$DCC JOBS=2 \
  bash scripts/kernel_pvh_verify.sh --build

# 3. realmode .S byte-for-byte vs gcc (expect: 14 passed, 0 failed; needs step 2's headers)
KERNEL_TREE=$TREE BUILD=$BUILD DCC_BIN=$DCC \
  bash scripts/kernel_pvh_verify.sh --partial

# 4. ELF checks
bash scripts/kernel_scan_jump_table.sh "$BUILD/vmlinux"   # expect: ok=5053 bad_nop=0
readelf -SW "$BUILD/vmlinux" | grep -E 'altinstr|jump_table'

# 5. boot — the judge is "Run /init as init process" on the serial console
KERNEL_TREE=$TREE BUILD=$BUILD bash scripts/kernel_pvh_verify.sh --boot

# 5b. runtime closure (§4.3): initramfs + ext4 image + module panel, then the gcc baseline
bash scripts/build_runtime_initramfs.sh        # runtime.cpio.gz + rt-ext4.img
bash scripts/runtime_closure.sh --modules      # configs.ko (multi-file) + hello.ko, dcc + modpost
bash scripts/runtime_closure.sh --gcc-build    # /root/pvh-gcc-rt, same .config, gcc SDK
bash scripts/runtime_closure.sh --stress       # -smp 4 + virtio-net, both kernels, cmp 26/26
bash scripts/runtime_closure.sh --audit        # every .cmd must name the dcc wrapper, not gcc

# 6. the §5.11 reproducer as a check (gcc here is a comparison baseline, not a build tool)
bash scripts/kernel_cc_asm_repro.sh
#   gcc -O2 : fail=0 ok=1 v=3 rc=0
#   dcc -O2 : fail=0 ok=1 v=3 rc=0   → 退出码 0 = 缺陷已修；非 0 就是 §5.11 回来了

# 7. the frame histogram of §5.12 (same 120 objects, prologue `sub rsp` only)
bash scripts/kernel_frame_hist.sh "$BUILD"        # 0x20 / 0x60 dominate (was 0x140…0x1b0)
bash scripts/kernel_frame_hist.sh /root/pvh-gcc-ref # 0x8…0x50 dominate

# 8. regression gates
cd /root/dcc
cargo test --release --no-fail-fast -- --test-threads=2   # 1 known C++ EH failure
JOBS=4 bash scripts/torture_test.sh all                   # expect: fail=0
```

Per-defect reproducers are the suite's own cases; `scripts/opt_pass_audit.sh` and
`scripts/frame_band_probe.sh` document theirs in their script headers.

## 7. What is not verified / known gaps

* **Boot to user space is proven; “identical to gcc” is proven for the runtime panel, not for every
  boot line.** A dcc-built image reaches `Run /init as init process`, runs the 17 `RT|` lines
  (`RT|basic` … `RT|ext4|magic roundtrip umount`, `RT|net|eth0 arp-complete`, `RT|exit|0`) plus
  the 9 `MOD|` lines, and all 26 are byte-identical to the gcc-built kernel — including the
  `-smp 4` + virtio-net round (§4.3). The comparison is on those lines, not on the whole dmesg:
  timings, addresses and banner strings differ by construction. With `-smp 4` the 16 KiB init
  stack is close to its guard page: 1 of 7 consecutive boots died in the `virtio_blk` probe
  chain (`__zone_watermark_unusable_free` entered with RSP = stack base), while the other 6
  produced the full panel and the gcc kernel never hit it. That is frame economy (§5.12), not
  a semantic difference; the stable multi-core evidence is the 26-line panel above.
* **Modules are verified out of tree only.** `make vmlinux` neither runs `modpost` nor links
  modules; `scripts/runtime_closure.sh --modules` builds `configs.ko` (multi-file) and `hello.ko`
  with the same compiler, `modpost` included, and the QEMU panel does `insmod` / proc read /
  `lsmod` / `dmesg` / `rmmod` / resident check. In-tree `modules_install` is not exercised, and the
  `__layout_sections` warning printed on insert is shared with the gcc kernel (§4.3).
* **`cleanup` destructors on the return path are emitted only for integer/`Memory` delivery**
  (§5.16): a function returning `Sse`/`SseUp`/`X87` (`float`/`double`, `__m128`, `long double`,
  complex) still skips function-body cleanups, because preserving `xmm0`/`st(0)` across the
  destructor `call` has no cheap form here and destroying a return value is the worse failure.
  Not on the kernel boot path, but a real gap.
* **Aggregate delivery has two documented exceptions** (§5.15): (a) `Expr::Cond` over a ≤ 8 B
  struct still delivers a register value, because `gen_expr_cond`'s materialisation gate says so
  (`cond_materializes_aggregate` is the single place both sides read), and (b) scalar-shaped
  types that SysV nevertheless classifies as aggregates (`__int128`, `_Complex`) are outside
  `is_aggregate_shape`, so the two argument sites in `abi/call.rs` keep an explicit
  `CompoundLit` arm for them.
* **`bzImage` / 16-bit C**: `arch/x86/boot/**` and `realmode/rm/{wakemain,video-mode}.c` are
  16-bit C and need a backend dcc does not have (`E0319` + `DCC-GAP`, asserted in §4.4). The PVH
  route bypasses them; the remaining 16-bit material is assembly and is byte-identical to gcc.
* **`COMPAT` / `vdso32`**: `-m32` C, switched off in this configuration.
* **Modules**: the module closure is out of tree (§4.3); this `localmodconfig` resolves to one
  in-tree module and `make vmlinux` does not run `modpost` for it.
* **C++**: `cpp_abi_eh_group` (six zero-cost-EH fixtures) still dies with a signal; it predates
  this cycle (§4.5) and is not on the kernel path.
* **Frame economy is not finished** (§5.12, §5.42): what is left of the floor is 16 B (the
  canary cell plus the variable area's first slot) + `temp_slots·8`; the callee-saved area is
  `popcount(plan)·8` now, and `FIXED_HDR = 48` survives only as the placeholder of an
  unresolved layout map. The `temp_slots ≥ 2` floor (16 B) still closes every frame at 32 B
  before the first local, where gcc often uses none.
* **Pre-existing debug-only defects, not fixed here** (they reproduce identically on a pristine
  `HEAD`, and wrap silently in the release build, which is why the release-profile cases do not
  see them): unsigned underflow in the brace-elision remaining-bytes computation
  (`gcc.c-torture/execute/20030305-1.c` — this one also mis-compiles under release and is fixed
  here with `saturating_*`), `container_bits - bit_o - bw` underflowing to a ≥ 64 shift in the
  `reverse_sso` bitfield store (`bitfield-1.c`, `bitfield-endian-1/2.c`), shift-count overflow in
  SCCP's `eval_binop` (`920702-1.c`, `dic.c`, `pr103314-1.c`, `pr105247.c`, `pr111059-*.c`),
  plus `mipscop-1..4` (E0316), `920521-1` (E0311), `asmgoto-5` (E0318/E1135) and `20090907-1`
  (E1136). None is on the kernel path; each deserves its own reduction.
* **Silent mis-binding is guarded, but only for the statically decidable shape**: E0321 compares
  a memory operand's resolved address against another output's write destination. Two outputs
  whose address is parked in a callee-saved register (`"=r"(a[i])`) can still alias at run time.
* **Pipeline audit is corpus-bound** (§5.13): the 0-hit rates are measured on 60 small torture
  units; kernel-sized TUs with IPA-shaped calls will move `ipa_modref` and `cross_*_dce`.
* **Both `cargo test` targets and `scripts/torture_test.sh all` were re-run on the final
  compiler** carrying §5.59 + §5.60 (`2026-11-12`: lib 2 612 passed / 0 failed / 2 ignored,
  integration 1 375 passed / 0 failed / 6 ignored; torture 3 707 passed / 0 failed / 6 skipped,
  which is also the pre-commit gate of the two commits). Both §5.59 and §5.60 pass at
  `-O0/-O1/-O2/-O3`. One exception is unchanged: `cpp_abi_eh_group` predates this cycle and is
  not on the kernel path (§4.5).
