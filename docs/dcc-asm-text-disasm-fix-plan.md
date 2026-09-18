# dcc -S 汇编文本错乱彻底优化方案

## 背景

当前 x86_64 后端的 `-S` 输出不是 codegen 原始汇编文本，而是先生成
机器码，再由 `src/codegen/disasm.rs` 的轻量内置反汇编器重新解码为
AT&T/GAS 文本。该设计可以复用现有直发机器码路径，但内置反汇编器不是
完整 x86_64 decoder，面对未覆盖指令时会出现文本错乱。

典型例子是小 vector return 修复后生成的真实机器码：

```asm
66 0f 6e c0    movd %eax,%xmm0
```

`objdump -d` 能正确识别为 `movd`，但当前 dcc `-S` 可能输出：

```asm
.byte 0x66
.byte 0x0f
.byte 0x6e
shift $144, -1869574000(%rax)
```

这不是 codegen 机器码错误，而是 `-S` 文本反汇编失步导致的展示错误。
因此，任何验证机器码语义的测试必须以 `objdump -d` 或等价成熟工具为准，
不能只检查 dcc `-S` 文本。

## 问题定义

### 直接症状

1. 未支持的 opcode 被拆成若干 `.byte`。
2. fallback 只消费一个字节，后续字节被当成新指令继续解码。
3. 结果可能出现不存在的伪指令，或者把正确机器码解释成错误文本。
4. `-S` 文本可能无法被 GAS 重新汇编，或重汇编后机器码不同。

### 根因

`disassemble_range()` 调用 `decode_one()` 逐条解码。当前逻辑是：

- 成功解码：输出文本并按返回长度推进位置。
- 失败解码：输出单字节 `.byte 0xNN`，只推进一个字节。

这对完整 decoder 是可接受兜底，但 dcc 的 decoder 覆盖面有限。未知指令
通常包含 prefix、opcode、ModRM、SIB、disp、imm 等多个字节；只消费首字节
必然导致位置失步。

### 影响范围

1. **正确性验证风险**：开发者可能被 `-S` 文本误导，误判机器码错误。
2. **回归测试风险**：基于字符串的 `-S` 测试可能错过真实机器码问题，也可能
   因展示错误误报。
3. **用户体验风险**：用户期望 `-S` 是可读、可汇编的汇编文件；错乱文本破坏
   该预期。
4. **维护风险**：每新增一个 codegen emit helper，都可能需要同步补
   disassembler，否则 `-S` 再次错乱。

## 目标

1. `-S` 输出必须稳定、可读，并且默认可由 GAS 重新汇编。
2. `-S` 输出与直发 `.o` 的 `.text` 机器码保持一致，允许符号/重定位文本形式
   差异，但最终机器码语义一致。
3. 未覆盖指令不得导致后续解码失步。
4. 测试体系明确区分“文本展示正确性”和“机器码语义正确性”。
5. 长期不再依赖手写完整 x86_64 反汇编器作为 `-S` 的唯一来源。

## 非目标

1. 不要求 dcc 自研完整替代 objdump/capstone/llvm-objdump。
2. 不要求一次性支持全部 x86_64 ISA 文本格式化。
3. 不改变 `.o` 直发机器码作为默认主路径。
4. 不把 inline asm 的完整 GAS 语义重新实现在 dcc 内部。

## 总体方案

采用三层方案彻底解决：

1. **主方案：引入 machine-code event log，直接记录 codegen 发射意图。**
   `-S` 不再依赖事后反汇编普通 codegen 指令，而是由发射时记录的结构化
   指令事件生成 GAS 文本。
2. **安全兜底：保留内置反汇编器，但必须实现未知指令整条消费。**
   任何 decoder 漏洞最多降级成整条 `.byte ...`，不能污染后续指令。
3. **验证 oracle：所有机器码语义验证使用 `objdump -d`。**
   `-S` 文本只用于文本可重汇编和展示一致性验证。

重构后架构：

```text
IR / AST lowering
      |
      v
x86_64 codegen emit helpers
      |
      +--> raw bytes in .text
      |
      +--> AsmEvent log: label/directive/instruction/data/fixup
                |
                v
          GAS-compatible -S formatter
```

`objdump -d` 仍作为测试和诊断 oracle：

```text
source.c
  |-- dcc -c --> object.o -- objdump -d --> machine-code oracle
  |
  `-- dcc -S --> output.s -- as --> output.o -- objdump -d
```

## 核心设计

### 1. AsmEvent 日志

新增 x86_64 汇编事件日志，记录 codegen 已经发射的语义，而不是从 bytes
反推语义。

建议结构：

```rust
pub enum AsmEvent {
    Section { name: String, flags: String },
    Global { symbol: String },
    Type { symbol: String, ty: AsmSymbolType },
    Size { symbol: String, expr: String },
    Label { symbol: String },
    Directive { text: String },
    Inst(AsmInst),
    Bytes { bytes: Vec<u8>, reason: ByteReason },
    Data(AsmData),
}

pub struct AsmInst {
    pub offset: u64,
    pub bytes_len: u8,
    pub mnemonic: AsmMnemonic,
    pub operands: Vec<AsmOperand>,
    pub prefixes: AsmPrefixes,
    pub comment: Option<String>,
}
```

事件的 `offset` 和 `bytes_len` 必须与真实 `.text` 发射保持同步，用于测试
断言和调试定位。

### 2. typed operand 表示

不要在 emit helper 中拼字符串。先保存结构化操作数，再由 formatter 统一输出
AT&T/GAS 格式。

```rust
pub enum AsmOperand {
    Reg { class: RegClass, index: u8, width: Width },
    Imm { value: i64, width: Width },
    Mem(AsmMem),
    Symbol { name: String, reloc: RelocKind },
    Label { name: String },
}

pub struct AsmMem {
    pub segment: Option<SegmentReg>,
    pub base: Option<Reg>,
    pub index: Option<Reg>,
    pub scale: u8,
    pub disp: i64,
    pub symbol: Option<String>,
    pub width: Option<Width>,
}
```

formatter 负责：

- AT&T 操作数顺序。
- `%rax` / `%eax` / `%xmm0` 命名。
- `disp(base,index,scale)` 内存格式。
- RIP-relative 符号格式。
- 指令后缀或无后缀形式。
- prefix，如 `lock`、`rep`、`data16`。

### 3. emit helper 统一记录事件

所有发射指令的 helper 改成“发 bytes + 记事件”的单入口，不允许有只发 bytes
而不记录文本语义的普通 codegen 指令。

例如：

```rust
fn emit_inst(&mut self, enc: EncodedInst, asm: AsmInst) {
    let start = self.pos();
    self.emit(&enc.bytes);
    debug_assert_eq!(self.pos() - start, asm.bytes_len as u64);
    if self.emit_asm_text {
        self.asm_events.push(AsmEvent::Inst(asm.with_offset(start)));
    }
}
```

低层 helper 可以逐步迁移：

- `emit_mov_imm_reg`
- `load_stack_sized`
- `store_stack_sized`
- `emit_xmm_r13_disp`
- `emit_movd_gp_to_xmm`
- call/jmp/branch helpers
- atomic 和 inline asm 相关 helpers

### 4. inline asm 与 global asm 处理

inline asm/global asm 有两类：

1. **dcc mini-asm 可编码子集**：继续编码成机器码，同时记录原始规范化 asm
   或结构化事件。
2. **未知/透传 GAS asm**：不尝试反汇编；在 `-S` 中保留原始文本，在 `.o`
   直发模式下按现有策略报错或走 host assembler fallback。

这能避免“GAS 文本 -> dcc 编码 -> dcc 反汇编 -> GAS 文本”的循环损耗。

### 5. reloc/fixup 文本化

当前 `disasm_calls`、`disasm_data` 以 offset map 给反汇编器补符号。重构后应将
符号引用作为 operand/fixup 直接挂在 `AsmInst` 上。

```rust
pub enum AsmFixup {
    PcRel32 { at: u64, target: String, next_ip: u64 },
    Abs64 { at: u64, symbol: String },
    GotPcRel { at: u64, symbol: String },
    Plt32 { at: u64, symbol: String },
}
```

formatter 根据 fixup 类型输出：

- `call foo`
- `jmp .Ltarget`
- `foo(%rip)`
- `foo@GOTPCREL(%rip)`
- `foo@PLT`

### 6. 内置 disassembler 的新定位

`src/codegen/disasm.rs` 保留，但不再作为 `-S` 主来源。它只承担：

1. 单元测试辅助。
2. debug dump。
3. 迁移期未事件化指令的临时 fallback。

同时必须增强 fallback：

- prefix 扫描完整。
- opcode map 至少知道是否存在 ModRM。
- ModRM/SIB/disp 长度正确跳过。
- imm 长度按 opcode table 跳过。
- VEX/EVEX 至少整条消费。
- 输出 `.byte 0x.., 0x.., ...`，而不是逐字节 `.byte`。

这样即使 mnemonic 未支持，也不会失步。

## 分阶段实施计划

### 阶段 0：建立基线和防线

目标：先防止错乱继续扩大，同时建立可验证基线。

任务：

1. 新增 `tests/disasm_oracle.rs` 或扩展 `tests/integration.rs` 的 helper。
2. 标准化 helper：`dcc -c -> objdump -d`，用于机器码断言。
3. 标准化 helper：`dcc -S -> as -> objdump -d`，用于 `-S` 可重汇编断言。
4. 加入已知失败用例：`66 0f 6e c0 movd`、`66 0f 7e /r`、常见 SSE load/store。
5. 在文档和测试命名中明确：机器码语义测试不得只依赖 dcc `-S` 文本。

验收：

- 现有 small vector return fixture 的机器码由 `objdump -d` 断言。
- 至少一个测试能稳定复现当前 `-S` 错乱，作为后续修复基线。

### 阶段 1：修复 fallback 失步

目标：未知指令最多不可读，不能污染后续文本。

任务：

1. 将 `decode_one()` 的失败结果从 `None` 改为结构化错误：
   `Unknown { start, len, reason }`。
2. 新增 `decode_unknown_len()`：根据 prefix/opcode/ModRM/SIB/disp/imm 计算整条长度。
3. 对 legacy、0F、0F38、0F3A、SSE prefix、VEX、EVEX 做最低限度长度表。
4. fallback 输出单行整条 bytes：`.byte 0x66, 0x0f, 0x6e, 0xc0`。
5. 禁止 fallback 只消费 1 字节，除非剩余字节不足以判定长度。

验收：

- `dcc -S` 对未知 `movd` 不再产生假 `shift`。
- 同一函数中未知指令后的 `nop`、`ret`、label 不再错位。
- `cargo test s_output_* disasm --test integration` 通过。

### 阶段 2：补齐当前 codegen 指令覆盖

目标：覆盖 dcc 自己会生成的 x86_64 指令，减少 `.byte` fallback。

优先级：

1. GP 基础：mov、lea、push/pop、call/jmp、jcc、setcc、cmov、alu、shift。
2. 窄宽转换：movzx/movsx/movsxd。
3. SSE scalar：movss/movsd、cvtsi2ss/sd、cvttss/sd2si、cvtss/sd。
4. SSE integer bridge：`movd`、`movq`、`punpck*`、`pxor` 等当前 vector 路径会用到的指令。
5. atomics：lock cmpxchg、xadd、mfence。
6. inline asm mini subset 已编码指令。

验收：

- dcc 自己生成的普通 C fixture `-S` 不出现 `.byte`，除非明确标注为未知 inline asm。
- small vector return 的 `-S` 文本正确显示 `movd %eax, %xmm0`。
- `-S -> as -> objdump -d` 与 `-c -> objdump -d` 在关键函数上等价。

### 阶段 3：引入 AsmEvent 日志并迁移核心 helper

目标：让 `-S` 从“反汇编机器码”切换为“格式化发射事件”。

任务：

1. 新增模块：`src/codegen/asm_event.rs` 或 `src/codegen/x86_64_asm.rs`。
2. 定义 `AsmEvent`、`AsmInst`、`AsmOperand`、`AsmMem`、`AsmFixup`。
3. 实现 AT&T/GAS formatter。
4. 将 x86_64 基础 helper 迁移到 `emit_inst()`：
   - return prologue/epilogue
   - stack load/store
   - GP move/alu
   - branch/call
   - small vector return 相关 XMM helper
5. `build_asm_output()` 优先使用 event log；对未迁移区间才调用 disasm fallback。
6. 增加 debug assertion：event offset/len 必须覆盖对应 emitted bytes。

验收：

- 核心 C 代码路径的 `-S` 不再依赖 `disasm::disassemble_range()`。
- event log 覆盖率统计达到 80% 以上。
- 未覆盖区间会显式输出注释：`# dcc-disasm-fallback`，便于继续迁移。

### 阶段 4：迁移 inline asm / section / directive

目标：让 `-S` 保留用户 asm 语义，不再二次反汇编破坏文本。

任务：

1. mini-asm 成功编码时记录规范化 asm event。
2. global asm 在 `-S` 中直接保留 directive/label/text。
3. `.pushsection/.popsection`、`.type/.size/.globl` 统一进入 event log。
4. 对 unsupported asm 明确分流：
   - `-S`：可保留原始 GAS 文本。
   - `-c`：按当前策略报错或 host assembler fallback。
5. 清理 `asm_directives_at`、`disasm_calls`、`disasm_data` 的重复职责。

验收：

- global asm 和 inline asm 的 `-S` 输出不经过反汇编损耗。
- section 排序、label 位置、`.size` 表达式稳定。
- 现有 asm 相关集成测试通过。

### 阶段 5：默认切换并收敛旧 disassembler

目标：完成 `-S` 主路径切换，旧 disassembler 只作为 debug/fallback。

任务：

1. `build_asm_output()` 默认只使用 event log formatter。
2. 新增内部调试选项，例如 `--debug-disasm-fallback`，用于比较旧反汇编输出。
3. 删除或降级旧 `disasm_calls/disasm_data` 主路径依赖。
4. 保留 `disasm.rs` 单元测试，但不再把它当作 `-S` 正确性核心。
5. 文档更新：开发验证机器码必须使用 `objdump -d`。

验收：

- `dcc -S` 输出可被 GAS 汇编。
- `dcc -S -> as` 与 `dcc -c` 的关键 `.text` 语义等价。
- 未事件化指令不会造成错误文本；最多输出整条 `.byte` 加注释。

## 测试策略

### 机器码语义测试

使用 `objdump -d` 验证，不看 dcc `-S` 文本。

适用：

- ABI 返回值。
- 调用约定。
- vector/SSE 指令宽度。
- atomic 指令。
- reloc/call/jump 目标。

### `-S` 文本测试

只验证文本输出质量：

- 是否含有预期 label/directive。
- 是否没有无意义错乱指令。
- 是否可由 GAS 汇编。
- 汇编后的 objdump 是否与直发对象关键片段一致。

### 差分测试

新增通用 helper：

1. `dcc -c -o direct.o input.c`
2. `dcc -S -o text.s input.c`
3. `as -o text.o text.s`
4. `objdump -d direct.o`
5. `objdump -d text.o`
6. 对指定 symbol 的指令 mnemonic/operand 归一化后比较。

不要直接 byte-for-byte 比整个对象文件，因为 symbol table、relocation、section
布局可能存在合法差异。

### fixture 分层

1. `tests/fixtures/c/disasm_basic.c`：基础 GP 指令。
2. `tests/fixtures/c/disasm_sse.c`：SSE scalar/vector bridge。
3. `tests/fixtures/c/disasm_control_flow.c`：call/jmp/jcc/label。
4. `tests/fixtures/c/disasm_inline_asm.c`：mini-asm/global asm/directive。
5. `tests/fixtures/c/vec_small_return.c`：继续作为 vector return 回归 fixture。

## 兼容性策略

1. 默认 `dcc -c` 行为不变，继续直发对象文件。
2. `dcc -S` 输出目标是 GAS-compatible assembly。
3. 对迁移期未覆盖指令，允许输出 `.byte`，但必须整条指令输出，且不得影响后续文本。
4. 对用户 inline asm，优先保留用户原始文本，避免 formatter 改写造成语义漂移。
5. 对 debug/test，可保留旧 disassembler 输出作为对照，但不要作为用户主输出。

## 风险与缓解

| 风险 | 缓解 |
| --- | --- |
| event log 与真实 bytes 不一致 | 每条 event 记录 offset/len，并 debug assert 与 emit 长度一致 |
| formatter 生成非法 GAS 语法 | `dcc -S -> as` 纳入集成测试 |
| 重定位文本化不完整 | fixup 从 codegen 原始 pending relocation 直接生成，不从 bytes 反推 |
| inline asm 被错误规范化 | 用户 asm 文本优先透传，只对 mini-asm 已知安全子集规范化 |
| 迁移范围大影响开发 | 先修 fallback 防失步，再逐步迁移 emit helper |
| 多架构实现差异 | 先完成 x86_64；其他架构沿用各自 asm_text_mode，后续复用 event 接口 |

## 推荐提交拆分

1. `disasm fallback: consume unknown instructions as a unit`
2. `x86 disasm: cover SSE movd/movq bridge instructions`
3. `tests: add objdump oracle for -S roundtrip`
4. `x86 asm: introduce structured asm event log`
5. `x86 asm: migrate core GP and stack emit helpers`
6. `x86 asm: migrate branch, call, and relocation formatting`
7. `x86 asm: migrate SSE/vector helper formatting`
8. `x86 asm: make event formatter the default -S backend`
9. `docs: document objdump-based machine-code verification`

## Definition of Done

1. `vec_small_return.c` 的 `dcc -c -> objdump -d` 显示 `movd`，不是错误的
   `movsd`。
2. 同一 fixture 的 `dcc -S` 文本显示正确 `movd`，且不会出现错乱 `shift`。
3. `dcc -S` 生成的 `.s` 可以由 GAS 重新汇编。
4. 重汇编对象经 `objdump -d` 后，与直发对象在目标函数级别语义等价。
5. 所有 `s_output_*`、asm、vector、x86_64 codegen 相关测试通过。
6. 文档和测试注释明确：机器码正确性以 `objdump -d` 为准。
7. 内置 disassembler 未覆盖指令时整条 `.byte` fallback，不再逐字节失步。
8. 新增 emit helper 必须同步记录 `AsmEvent` 或明确标注 fallback。

## 结论

彻底解决该问题不能只靠继续给 `disasm.rs` 补 opcode。补 opcode 能修单点，
但无法消除“自研轻量反汇编覆盖不足导致 `-S` 失步”的结构性问题。

推荐路径是：先修 fallback 防失步并补齐当前缺口；随后引入结构化
`AsmEvent` 日志，让 `-S` 从 codegen 发射意图直接格式化生成；最终把
`objdump -d` 固化为机器码语义 oracle。这样既能保证用户看到稳定可汇编的
`-S` 文本，也能避免未来新增指令时再次出现文本反汇编错乱。
