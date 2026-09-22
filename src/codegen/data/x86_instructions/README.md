# x86_64 Instruction Specifications

This directory contains JSON instruction specifications for the x86_64 architecture.

## Overview

These specifications define the encoding format, operands, and semantics for core x86_64 instructions used in the DCC compiler's data-driven encoding layer.

### Core Instructions Included:

| Instruction | Description | Files | Formats |
|-------------|-------------|-------|---------|
| **add** | Integer addition | `add.json` | 3 formats |
| **sub** | Integer subtraction | `sub.json` | 2 formats |
| **mul** | Integer multiplication | `mul.json` | 2 formats |
| **and** | Bitwise AND | `and.json` | 2 formats |
| **or** | Bitwise OR | `or.json` | 2 formats |
| **xor** | Bitwise XOR | `xor.json` | 2 formats |

## Usage

The code generator reads these JSON files and produces Rust code for efficient instruction encoding.

### Generate Code

```bash
cd /root/dcc/scripts/codegen-gen
cargo run -- \
  --input-dir ../../src/codegen/data/x86_instructions \
  --output-dir ../../src/codegen/generated \
  --isa x86_64
```

### Validate Specs

```bash
cargo run -- \
  --input-dir ../../src/codegen/data/x86_instructions \
  --validate-only
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

## Extending the Instruction Set

To add a new instruction:

1. Create a new JSON file following the schema
2. Define all encoding formats needed
3. Specify operand types and directions
4. Document flag modifications and side effects
5. Test with codegen-gen

### Example Template

```json
{
  "$schema": "../instruction_spec_schema.json",
  "name": "new_instr",
  "description": "Description of instruction",
  "arch": "x86_64",
  "operands": [...],
  "encoding_spec": {
    "formats": [...]
  },
  "semantics": {
    "effect": "...",
    "flags_modified": [...]
  }
}
```

See existing specs for examples.

## Next Steps

After generating code:

1. Review generated `src/codegen/generated/x86_64_instructions.rs`
2. Update backends to use generated encoders
3. Add tests for each instruction format
4. Verify against hardware reference

## References

- [Intel Manual Volume 2](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html) - Instruction set reference
- [AMD Manual](https://www.amd.com/system/files/TechDocs/24594.pdf) - Additional details
- [JSON Schema](../../instruction_spec_schema.json) - Specification format
