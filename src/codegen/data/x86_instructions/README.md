# x86_64 Instruction Specifications (TOML Format)

This directory contains TOML instruction specifications for the x86_64 architecture.

## Overview

These specifications define the encoding format, operands, and semantics for core x86_64 instructions used in the DCC compiler's data-driven encoding layer.

### Core Instructions Migrated ✅

| Instruction | Description | Formats | Status |
|-------------|-------------|---------|--------|
| **add** | Integer addition | 3 formats | ✅ Complete |
| **sub** | Integer subtraction | 2 formats | ✅ Complete |
| **mul** | Integer multiplication | 2 formats | ✅ Complete |
| **and** | Bitwise AND | 2 formats | ✅ Complete |
| **or** | Bitwise OR | 2 formats | ✅ Complete |
| **xor** | Bitwise XOR | 2 formats | ✅ Complete |
| **cmp** | Compare/implicit subtract | 3 formats | ✅ Complete |
| **mov** | Move data | 2 formats | ✅ Complete |

## TOML vs JSON Migration

All instructions have been migrated from JSON to TOML format for:
- **Strong typing**: Compile-time type safety via serde derive macros
- **Human readability**: INI-like syntax is more intuitive than JSON
- **Comment support**: Inline documentation with `#` comments
- **Type constraints**: Enumerated values enforced by schema

## Usage

Load and use instruction specifications in your code:

```rust,no_run
use dcc::codegen::data::*;

// Load all instructions
let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");
let specs = loader.load_all_instructions()?;

// Get specific instruction
let add_instr = specs.get("add").unwrap();

// Select best encoding for given operands
let selector = SmartEncodingSelector::new();
let operands = vec![
    OperandAnalysis { 
        op_type: "reg".to_string(), 
        size_bits: Some(64),
        is_readonly: false, 
        is_writeonly: true, 
        is_readwrite: false,
    },
    OperandAnalysis { 
        op_type: "reg".to_string(), 
        size_bits: Some(64),
        is_readonly: true, 
        is_writeonly: false, 
        is_readwrite: false,
    },
];

let selected = selector.select_format(add_instr, &operands)?;

// Emit machine code bytes
let emitter = ByteEmitter::new();
let bytes = emitter.emit(&selected)?;
println!("Machine code: {}", bytes_to_hex(&bytes));
```

## Architecture Details

### Encoding Features

- **ModR/M Bytes**: Most instructions require ModR/M byte for operand addressing
- **REX Prefixes**: Used for 64-bit registers (R8-R15, etc.)
- **Addressing Modes**: Supports all standard x86 addressing modes
- **Immediate Sizes**: Handles 8-bit, 16-bit, and 32-bit immediates

### Operand Types Supported

- `regmem`: Register or memory location
- `regmemimmediate`: Register, memory, or immediate value
- `reg`: Register only
- `immediate`: Immediate constant

### Available Encoding Patterns

- `"r/m r"` - Register/memory to register
- `"imm r/m"` - Immediate to register/memory
- `"r r/m"` - Register to register/memory
- `"imm al"` - Immediate to accumulator (special case)

## Extending the Instruction Set

To add a new instruction:

1. Create a new TOML file following the template structure
2. Define all encoding formats needed
3. Specify operand types and directions
4. Document flag modifications and side effects
5. Test with cargo test

### Example Template

```toml
[package]
name = "my_instruction"
version = "1.0.0"
description = "Description here"

arch = "x8664"

[[operands]]
name = "dest"
op_type = "reg"
direction = "write"

[[operands]]
name = "src"
op_type = "reg"
direction = "read"

[encoding_spec]
default_format = "RegReg"

[[encoding_spec.formats]]
name = "RegReg"
operand_pattern = "r r"
opcode = [0xC7]
modrm_required = true
```

See existing specs for complete examples.

## Testing

Run comprehensive tests:

```bash
cargo test --release data::
```

Expected test coverage:
- ✅ TOML loading validation
- ✅ Add instruction encoding
- ✅ Smart encoding selection
- ✅ Byte emission correctness
- ✅ Full pipeline integration

## Next Steps

After migrating all instructions:

1. Review generated encodings against Intel Manual
2. Validate confidence scores for each format selection
3. Implement advanced ModR/M/SIB encoding details
4. Add golden baseline hex dumps for verification
5. Integrate with codegen-gen toolchain

## References

- [Intel SDM Volume 2](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html) - Instruction set reference
- [Data-Driven Encoding Quick Ref](../../../DATA_DRIVEN_ENCODING_QUICK_REF.md) - Developer guide
- [TOML Migration Report](../../../TOML_MIGRATION_AND_SMART_ENCODING_COMPLETE.md) - Implementation summary

*Last updated: 2026-09-22 - All 9 instructions migrated to TOML format*
