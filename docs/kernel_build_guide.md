# DCC 编译 Linux 内核（x86_64）操作手册

范围：用 dcc 编 Linux 7.1 / x86_64 的 `localmodconfig` 内核并起进 QEMU。
验收标准是**内核编译与链接全程不许出现 gcc**，产物用 PVH 直接引导到用户空间 `init`。
缺陷清单、实测数据与验证结论都在 [dcc-kernel-build-verify-report.md](dcc-kernel-build-verify-report.md)，本手册只写怎么做。

## 1. 前置

| 项 | 值 |
|---|---|
| 内核源码 | 纯净发布树，本手册示例用 `/root/linux-pvh-src`（不带 git 元数据、不打本地补丁） |
| 依赖 | `build-essential make bc flex bison libssl-dev libelf-dev`；引导验证需 `qemu-system-x86_64` |
| dcc | 本仓库 `cargo build --release -j2` → `target/release/dcc` |
| 并行度 | 一律 `-j2` / `--test-threads=2`（内存小的机器上更高会 OOM） |
| 磁盘 | 全量构建 + include 缓存可达数十 GB，见 §7 |

gcc 在这里只有一个身份：**对照基线**（`--partial` 的逐字节对拍、产物差异判定）。它不是构建工具，
构建里一旦被起起来，验收装置的 gcc 陷阱会以退出码 99 当场失败。

## 2. 四阶段验收装置

一切从 `scripts/kernel_pvh_verify.sh` 走，四个阶段可单独跑：

```bash
export KERNEL_TREE=/root/linux-pvh-src BUILD=/root/pvh-lmc3 DCC_BIN=/root/dcc/target/release/dcc

bash scripts/kernel_pvh_verify.sh --config   # 落配置（O= 外建，源码树不动）
bash scripts/kernel_pvh_verify.sh --build    # 全树 make vmlinux，PATH 上摆 gcc 陷阱
bash scripts/kernel_pvh_verify.sh --partial  # realmode 六个 .S 与 gcc -m16 逐字节对拍
bash scripts/kernel_pvh_verify.sh --boot     # QEMU -kernel vmlinux，验到 Run /init
```

环境变量：`JOBS`（默认 2）、`KCONFIG`（默认 `localmodconfig`，压力项可传 `allmodconfig`、
快速通道 `x86_64_defconfig`）、`BUILD_TARGET`（默认 `vmlinux`；要模块就传 `all`）、
`QEMU_TIMEOUT`（默认 45 秒）。`--all` 依次跑四步。

配置里必须成立的四条：`CONFIG_PVH=y`、`CONFIG_ACPI_SLEEP` / `CONFIG_IA32_EMULATION` /
`CONFIG_COMPAT` 全关。关掉这三项正是为了让 16/32 位 C 语料（`arch/x86/boot/**`、
`realmode/rm/**`、vdso32）不进这条链 —— dcc 没有 16 位 C 后端（见 §8）。配置用
`$(KERNEL_TREE)/scripts/config` 改，不用 `sed`：`ACPI_SLEEP` 没有 prompt，`sed` 写进去会被
`olddefconfig` 按 `default y` 拉回来。

`--build` 自带两条可证伪的判据，输出直接印在终端：

```
gcc traps:  0        # PATH 陷阱目录里 gcc/cc/g++/c++/clang/ld.bfd 被起过几次
delegates:  0        # 包装器日志里的 GCC-DELEGATE 行数
```

最终链接仍由宿主 `ld` 完成 —— dcc 产出可重定位目标文件并直接驱动链接器，它不是链接器，
这与 gcc 构建里 `gcc → as → ld` 的分工等价。

## 3. 手工等效命令

验收装置里这两条命令是全部实质，需要绕过脚本时照抄：

```bash
# 配置：与构建用同一个 CC，否则 kbuild 会按 $(CC) --version 重写 .config（见 §5.5）
make -C "$KERNEL_TREE" O="$BUILD" CC=/root/dcc/scripts/dcc-kernel-cc.sh \
     HOSTCC=/root/dcc/scripts/dcc-host-wrapper.sh HOSTLD=ld defconfig
make -C "$KERNEL_TREE" O="$BUILD" CC=… HOSTCC=… localmodconfig < <(yes "")
"$KERNEL_TREE/scripts/config" --file "$BUILD/.config" \
    -e PVH -d SUSPEND -d HIBERNATION -d ACPI_SLEEP -d IA32_EMULATION -d COMPAT
make -C "$KERNEL_TREE" O="$BUILD" CC=… HOSTCC=… olddefconfig

# 构建
make -C "$KERNEL_TREE" O="$BUILD" -j2 \
     CC=/root/dcc/scripts/dcc-kernel-cc.sh \
     HOSTCC=/root/dcc/scripts/dcc-host-wrapper.sh HOSTLD=ld vmlinux
```

`CC`/`HOSTCC` 的值会被按 shell 词法拆分，所以带参数的包装器可以直接写。宿主工具（`scripts/`、
`tools/`）也走 dcc：`dcc-host-wrapper.sh` 是 `dcc -c` + 直接驱动 `ld`；`fixdep` 另有 Rust 版
退路（`scripts/build_fixdep_dcc.sh`）。

## 4. 单个翻译单元重放

全树日志只能告诉你哪个对象红了。要只看一个 TU，用构建树里现成的 `.cmd` 指纹重放：

```bash
bash scripts/repro_kbuild_tu.sh /root/pvh-lmc3 arch/x86/kernel/apic/apic.o /tmp/apic.o
```

它读 `$BUILD/<dir>/.<obj>.cmd` 里的 `savedcmd`，截掉 objtool 段，改写 `-o` 后在 `$BUILD` 目录下执行。
要把源文件换成自己写的复现件（借同一整套内核 include 路径与宏）：

```bash
bash scripts/dcc_kernel_tu.sh /root/pvh-lmc3/t.c /tmp/t.o /root/pvh-lmc3
```

拿预处理产物缩小现场：把 `savedcmd` 里的 `-c -o …` 换成 `-E -o /tmp/x.i`，得到完整宏展开的单一文件。
注意 dcc 与 kbuild 一样按 `srctree` 解析源路径，替换源文件时要把**整段绝对路径**换掉，
留下 `/root/linux-pvh-src/` 前缀会得到 `cannot read /root/linux-pvh-src//tmp/x.c`。

## 5. 改完 dcc 之后怎么接着跑

### 5.1 标准三步（顺序不可颠倒）

```bash
cd /root/dcc && cargo build --release -j2                     # 1) 只重编二进制
bash scripts/kernel_purge_stale_objects.sh "$KERNEL_TREE" /root/dcc/target/release/dcc   # 2) 作废旧产物
make -C "$KERNEL_TREE" O="$BUILD" -j2 \
     CC=/root/dcc/scripts/dcc-kernel-cc.sh \
     HOSTCC=/root/dcc/scripts/dcc-host-wrapper.sh HOSTLD=ld vmlinux   # 3) 同一棵树续跑
```

kbuild 的 `if_changed` 只比命令行字符串，句子里写的是包装器路径 —— 编译器换版它不认，
旧 `.o` 会原样留在 `vmlinux` 里。所以**先 purge 再 make**；只做第 3 步会得到一棵说不清版本、
也不可复现的混版树。purge 不是 `make clean`：`include/generated`、宿主工具、`.config` 都保留。

### 5.2 只影响一部分对象时按签名定向作废

| 模式 | 判据 | 用于 |
|---|---|---|
| 默认 | `.o` 的 mtime 早于基准二进制，或带 gcc 的 `.comment` | 编译器一换就全量 |
| `--gcc-fallback-only` | 只看 `.comment=GCC` | 改动不改产物字节 |
| `--deps <子串>` | `.o.cmd` 的依赖表里出现该头文件 | 头文件里的宏 / inline 被改对 |
| `--src <子串>` | `.o.cmd` 里出现该源文件 | 单个 TU |
| `--insn <ERE>` | 现有 `.o` 的反汇编命中该正则 | 编码层缺陷（产物里有可识别的坏形状） |

`--list` 与任一模式合用先看命中集。两条前提：改动在**未命中集**上不产生字节差异；
签名能从产物里完整判定。命中集接近全树时别硬做定向，直接回默认模式。

宿主工具与有意交给 gcc 的 16 位产物（`arch/x86/boot/*.o`、`realmode/rm/*`、`rmpiggy.o`）
不参与判据、不许删；`arch/x86/boot/startup/`、`boot/compressed/`、`realmode/init.o` 是普通
64 位 TU，照常参与。

### 5.3 全树构建期间要用只读二进制快照

全树 make 与手工实验共用 `target/release/dcc` 时，实验每重编一次二进制，树里就多一版对象。
给这一轮树固定一个快照路径：

```bash
mkdir -p /root/dcc-tree-snap            # 路径自定，重点是这一轮树只认它
install -m 0755 target/release/dcc /root/dcc-tree-snap/dcc
bash scripts/kernel_purge_stale_objects.sh "$KERNEL_TREE" /root/dcc-tree-snap/dcc
# 之后实验随便重编 target/release/dcc；要把改动入树时重装快照再 purge
```

杀后台构建要杀真正的 make 进程组，不是包装 bash：

```bash
ps -eo pid,pgid,comm,args | awk '$3=="make" && $NF ~ /^CC=\/root\/dcc/ {print $2; exit}'
```

`args` 字段本身以命令名开头，所以 `CC=…` 落在 `$NF` 而不是 `$5`。日志里 `kill` 造成的
`*** … 已终止` 不是编译失败，只有 `错误 1` / `error[Exxxx]` 才是。

### 5.4 测试链必须单例

`cargo test` 会重写 `target/release/dcc`，两条链并行时第二条会排在 cargo 的构建锁后面，
拿到的是「lib 是上一版源码、用例是这一版」的混版判据。开链前确认没有别的 cargo：

```bash
pgrep -af 'bin/cargo' | head        # arg0 是全路径，^cargo 匹配不到
```

### 5.5 一个 `O=` 目录只认一个 CC

在同一个 `O=` 目录上用别的 CC 跑任何 `make`（哪怕只是 `prepare0` 重编宿主工具），
syncconfig 都会按新 CC 重写 `CONFIG_CC_VERSION_TEXT` 及其派生旗标，下一轮全树从
`include/generated` 起全部过期。要 gcc 版做对照就另开独立树（本项目用
`GCC_BUILD=/root/pvh-gcc-rt`，`scripts/runtime_closure.sh --gcc-build` 会断言它与 `BUILD`
和源码树不是同一棵）。
判本轮是不是增量只看日志开头：出现 `SYNC include/config/auto.conf.cmd` 就是从 prepare 重来。

混进来的 gcc 对象不会让构建失败——kbuild 只比命令行字符串，`.cmd` 里写着 gcc 的对象就一直是
gcc 产物。判断时**不要 grep `gcc` 子串**：`-I/usr/lib/gcc/…` 会让几乎每一行命中（实测 565 个
误报）。跑 `bash scripts/runtime_closure.sh --audit`：它取每个 `.cmd` 的 `savedcmd_` 行，只看
`:=` 后的首个 token，`gcc` / `gcc-N` / `cc1` / `cc1plus` 即判污染。
`arch/x86/kernel/{cet,ibt_selftest}.o` 就是这样在定向编译时留在树里的。

## 6. 产物检查

```bash
bash scripts/kernel_scan_jump_table.sh "$BUILD/vmlinux"     # 期望 ok=N bad_nop=0
bash scripts/kernel_frame_hist.sh "$BUILD"                  # 序言帧分布（对照 gcc 树）
bash scripts/kernel_cc_asm_repro.sh                         # @cc 不可见输出绑定检查
bash scripts/runtime_closure.sh --audit                     # 每个 .cmd 的首 token 必须是 dcc 包装器
bash scripts/runtime_closure.sh --stress                    # -smp 4 + virtio-net，两侧 26 行 RT|/MOD| 逐字节比
readelf -SW "$BUILD/vmlinux" | grep -E 'altinstr|jump_table'
```

objtool 是链接路径的一部分，每个对象都过；它的 `error` 会直接让 `--build` 失败，
不需要单独跑。反汇编某个符号：

```bash
nm "$BUILD/vmlinux" | awk '$3=="apic_set_fixmap"{print $1}'
objdump -d --start-address=0x<地址> --stop-address=0x<地址+宽> -Mintel "$BUILD/vmlinux"
```

## 7. 磁盘

```bash
bash scripts/cleanup_disk.sh --dry          # 先看占用，不删
bash scripts/cleanup_disk.sh                # 安全档（1–6，不动 target/）
bash scripts/cleanup_disk.sh --only 1       # 只清 ~/.dcc 缓存（可达数十 GB）
bash scripts/cleanup_disk.sh --cache-prune-minutes 90   # 构建中途腾磁盘：只清 90 分钟没写过的条目
```

构建进行中**不要**跑默认档：第 6 项会整棵清空 `.work_tmp/`，那里放着调试现场；
也不要整删缓存（正在命中的条目会全部重算），按 mtime 切掉旧条目只造成 miss，
不影响正确性。`df -h /` 每轮复跑。

## 8. 已知限制

* **16 位 C 没有后端**：`-m16` 的 C 输入（`arch/x86/boot/**`、`realmode/rm/wakemain.c`、
  `video-mode.c`）报 `E0319` 并留 `DCC-GAP reason=m16/m32-no-c-backend` 标记，
  且**不会**回退 gcc（`--partial` 断言这一点）。因此验收走 PVH，需要 `bzImage` 时得先补这个后端。
* **`-m32` 同理**，`COMPAT` / `vdso32` 不在验收配置里。
* **模块侧不在这条链上**：`make vmlinux` 不跑 `modpost`，也不链 `.ko`。
* 编译器侧的能力矩阵见 [support_features.md](support_features.md)。

## 9. 诊断开关

只打印，不参与任何判据，也不改变产物：

`DCC_OPT_SKIP=<pass>`（跳过单个优化 pass，反向验证杠杆）、`DCC_FOLD_TRACE=1`、
`DCC_DUMP_LEFTOVER=1`、`DCC_DCE_TRACE=<fn>`、`DCC_EMIT_TRACE=1`、`DCC_TRACE_SKIP=1`（`.skip` 测距）、
`DCC_DEBUG_PACK=1`（include 缓存的 pack 状态）。

`DCC_SKIP_AI_BODY=1` 只能用于排障：它把「该展开却展不开」的形状从编译错误变成链接错误，不能当修复用。

## 10. 相关文档

* [dcc-kernel-build-verify-report.md](dcc-kernel-build-verify-report.md) — 实测结果、缺陷账目、复现链
* [support_features.md](support_features.md) — 能力与缺口矩阵
* `scripts/kernel_pvh_verify.sh` 头部注释 — 为什么走 PVH、四阶段各自做什么
