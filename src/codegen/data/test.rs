// src/codegen/data/test.rs - Test module for TOML-based instruction specs

#[cfg(test)]
mod tests {
    use crate::codegen::data::*;

    #[test]
    fn test_toml_instruction_loader() {
        let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");

        // Load all instructions
        let specs = loader
            .load_all_instructions()
            .expect("Failed to load TOML specs");

        assert!(specs.contains_key("add"), "Should contain 'add' instruction");
        assert!(specs.contains_key("mov"), "Should contain 'mov' instruction");

        println!("✓ Loaded {} instructions from TOML files", specs.len());
    }

    #[test]
    fn test_add_instruction_encoding() {
        let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");
        let add_instr = loader.load_instruction("add").expect("Failed to load add");

        assert_eq!(add_instr.name, "add");
        assert_eq!(add_instr.arch, "x8664");
        assert_eq!(add_instr.encoding_spec.formats.len(), 3);

        // Check format names
        let format_names: Vec<&str> = add_instr
            .encoding_spec
            .formats
            .iter()
            .map(|f| f.name.as_str())
            .collect();

        assert!(format_names.contains(&"RegMemReg"));
        assert!(format_names.contains(&"RegImm"));
        assert!(format_names.contains(&"AccumulatorImm"));

        println!("✓ Add instruction has {} formats", add_instr.encoding_spec.formats.len());
    }

    #[test]
    fn test_smart_encoding_selector() {
        let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");
        let add_instr = loader.load_instruction("add").expect("Failed to load add");

        let selector = SmartEncodingSelector::new();

        // Create operand analysis for register-to-register ADD
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

        let selected = selector
            .select_format(&add_instr, &operands)
            .expect("Failed to select format");

        println!(
            "✓ Selected format: '{}' with confidence {:.2}",
            selected.format.name, selected.confidence
        );
        assert!(selected.confidence > 0.5, "Should have high confidence match");
    }

    #[test]
    fn test_byte_emitter_basic() {
        let emitter = ByteEmitter::new();

        // Create a simple test format
        let format = EncodingFormat {
            name: "test_reg".to_string(),
            description: None,
            operand_pattern: "r r".to_string(),
            opcode: vec![0xC7, 0xC0],
            modrm_required: true,
            rex_prefix: false,
            escape_bytes: None,
            addressing_mode: None,
            extra_bytes: false,
            special_case: false,
        };

        let selection = SelectedFormat {
            format,
            bindings: vec![],
            confidence: 1.0,
        };

        let bytes = emitter.emit(&selection).expect("Failed to emit bytes");

        // Should emit opcode + ModR/M byte
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes[0], 0xC7);
        assert_eq!(bytes[1], 0xC0);

        println!("✓ Emitted {:?} bytes: {}", bytes.len(), bytes_to_hex(&bytes));
    }

    #[test]
    fn test_full_pipeline() {
        // Full pipeline: Load → Select → Emit
        let mut loader = InstructionLoader::new("src/codegen/data/x86_instructions");
        let mov_instr = loader.load_instruction("mov").expect("Failed to load mov");

        let selector = SmartEncodingSelector::new();
        let emitter = ByteEmitter::new();

        // Simulate MOV Rd, Rx (register to register)
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

        let selected = selector
            .select_format(&mov_instr, &operands)
            .expect("Failed to select");
        let bytes = emitter.emit(&selected).expect("Failed to emit");

        println!("✓ Full pipeline successful: {} bytes", bytes.len());
        println!("  Format: {}, Confidence: {:.2}", selected.format.name, selected.confidence);
        println!("  Machine code: {}", bytes_to_hex(&bytes));
    }
}
