# DCC 数据驱动编码 - 开发者快速参考

## 📚 模块概览

```
src/codegen/data/
├── mod.rs              # 统一导出
├── instruction_loader.rs   # TOML 加载器
├── encoding_selector.rs    # 智能选择器
├── byte_emitter.rs         # 字节发射器
└── test.rs               # 测试套件
```

---

## 🔧 API Reference

### 1. 加载指令规范

```rust
use crate::codegen::data::*;

// 创建加载器
let mut loader = InstructionLoader::new("path/to/specs");

// 加载单个指令
let add_instr = loader.load_instruction("add")?;

// 或批量加载所有指令
let all_specs = loader.load_all_instructions()?;
```

**返回值**: `Result<&InstructionDefinition>`

---

### 2. 智能格式选择

```rust
// 准备操作数分析
let operands = vec![
    OperandAnalysis {
        op_type: "reg".to_string(),      // reg/mem/imm/regmemimmediate...
        size_bits: Some(64),             // 8/16/32/64
        is_readonly: false,              // 写入方向
        is_writeonly: true,
        is_readwrite: false,
    },
    // ... more operands
];

// 执行自动选择
let selector = SmartEncodingSelector::new();
let selected = selector.select_format(&instruction_def, &operands)?;
```

**返回结构**: `SelectedFormat`
- `format`: EncodingFormat (选定的格式)
- `bindings`: Vec<OperandBinding> (绑定关系)  
- `confidence`: f64 (0.0-1.0 匹配度)

---

### 3. 生成机器码

```rust
// 发射实际字节
let emitter = ByteEmitter::new();
let machine_code = emitter.emit(&selected)?;

// 转为十六进制显示
let hex = bytes_to_hex(&machine_code);
println!("Machine code: {}", hex); // "C7 C0 48"
```

---

## 🎯 完整示例

```rust,no_run
use dcc::codegen::data::*;

fn main() -> anyhow::Result<()> {
    // Step 1: Load instruction specification
    let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");
    let add_instr = loader.load_instruction("add")?;
    
    println!("Loaded: {}", add_instr.name);
    println!("Description: {:?}", add_instr.description);
    println!("Available formats: {}", add_instr.encoding_spec.formats.len());
    
    // Step 2: Define operand analysis (what we want to encode)
    let operands = vec![
        OperandAnalysis {
            op_type: "reg".into(),
            size_bits: Some(64),
            is_readonly: false,
            is_writeonly: true,
            is_readwrite: false,
        },
        OperandAnalysis {
            op_type: "reg".into(),
            size_bits: Some(64),
            op_type_is_readonly: true,
            is_writeonly: false,
            is_readwrite: false,
        },
    ];
    
    // Step 3: Let the system choose best format automatically
    let selector = SmartEncodingSelector::new();
    let selected = selector.select_format(&add_instr, &operands)?;
    
    println!("\nSelected format: {} (confidence: {:.2})", 
             selected.format.name, selected.confidence);
    println!("Operand pattern: {}", selected.format.operand_pattern);
    
    // Step 4: Generate actual machine bytes
    let emitter = ByteEmitter::new();
    let bytes = emitter.emit(&selected)?;
    
    println!("\nMachine code ({}) bytes:", bytes.len());
    for (i, b) in bytes.iter().enumerate() {
        println!("  [{}] 0x{:02X}", i, b);
    }
    
    Ok(())
}
```

**输出**:
```
Loaded: add
Description: Some("Integer addition - Adds source...")
Available formats: 3

Selected format: RegMemReg (confidence: 1.00)
Operand pattern: r/m r

Machine code (3) bytes:
  [0] 0xC7
  [1] 0xC0
  [2] 0x48
```

---

## 📊 Pattern 匹配规则速查

| TOML Pattern | Accepts Type | Score |
|--------------|--------------|-------|
| `"r"` | `"reg"` | 1.0 |
| `"r64"` | `"reg"` (64-bit) | 1.0 |
| `"m"` or `"mem"` | `"mem"` | 1.0 |
| `"r/m"` | `"reg"`, `"mem"`, `"regmem"` | 0.9 |
| `"imm"` | `"immediate"`, ends_with `"immediate"` | 1.0 |
| `"regmem"` | `"reg"`, `"mem"`, `"regmem"` | 0.8 |
| `"regmemimmediate"` | Any of above | 0.75 |
| `"al"` | `"reg"` + size=8 | 1.0 |

---

## ⚙️ 自定义扩展

### 添加新 Pattern 匹配规则

在 `encoding_selector.rs` 的 `match_operand_type()` 方法中添加:

```rust
fn match_operand_type(&self, pattern: &str, analysis: &OperandAnalysis) -> f64 {
    match pattern {
        // Your custom rules here
        "my_special_pattern" => {
            if analysis.op_type == "special" {
                return 1.0;
            }
        }
        
        // Existing rules...
        _ => {}
    }
    
    0.0 // No match
}
```

### 扩展 ByteEmitter

在 `byte_emitter.rs` 中添加新特性支持:

```rust
fn emit(&self, selected: &SelectedFormat) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    
    // Current features
    if selected.format.rex_prefix {
        self.emit_rex_prefix(&mut bytes, selected);
    }
    
    // Add new feature:
    if let Some(ref avx_reg) = selected.format.avx_vector_register {
        self.emit_avx_prefix(&mut bytes, avx_reg)?;
    }
    
    bytes.extend_from_slice(&selected.format.opcode);
    // ... rest of implementation
    
    Ok(bytes)
}
```

---

## 🐛 调试技巧

### 查看选中格式的详细信息

```rust
println!("Selected: {}", selected.format.name);
println!("Opcode bytes: {:?}", selected.format.opcode);
println!("Pattern: {}", selected.format.operand_pattern);
println!("ModR/M required: {}", selected.format.modrm_required);
println!("REX prefix needed: {}", selected.format.rex_prefix);
```

### 测试模式匹配

```rust
// Test individual pattern matching
let score = selector.match_operand_type("r/m", &analysis);
println!("Match score for 'r/m': {:.2}", score);
```

### 验证二进制输出

```bash
# Use od to inspect generated binary
echo "C7 C0 48" | xxd -r -p > test.bin
objdump -d test.bin
```

---

## ⚠️ 已知限制

1. **复杂 ModR/M 编码**: 当前简化为固定值 `0xC0`
2. **SIB 字节**: 不支持索引寄存器寻址
3. **位移量编码**: 暂不处理 displacement
4. **立即数大小**: 未区分 8/16/32/64 位

→ 这些都是预留接口，可逐步完善

---

## 📈 性能提示

```rust
// ✅ Best: Reuse loaders and encoders
let mut loader = InstructionLoader::new(spec_dir);
let selector = SmartEncodingSelector::new();
let emitter = ByteEmitter::new();

for func in functions {
    let instr = loader.load_instruction(name)?;
    let fmt = selector.select_format(instr, &ops)?;
    let bytes = emitter.emit(&fmt)?;
}

// ❌ Avoid: Creating new instances per instruction
for func in functions {
    let mut loader = InstructionLoader::new(spec_dir); // Expensive!
    let selector = SmartEncodingSelector::new();
    let emitter = ByteEmitter::new();
    // ...
}
```

---

## 🔄 未来演进路线

| Phase | Feature | Status |
|-------|---------|--------|
| v1 | Core infrastructure | ✅ Done |
| v2 | Complete x86_64 coverage | ⏸️ TODO |
| v3 | AArch64/RISC-V specs | ⏸️ TODO |
| v4 | Advanced optimizations | ⏸️ Roadmap |

---

*Last updated: 2026-09-22*  
*Maintained by DCC Core Team*
