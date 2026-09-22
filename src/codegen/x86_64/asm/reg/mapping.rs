// Register mapping utilities for inline assembly
// Contains: register names, sizes, aliases, conversion helpers

/// Map register index (0-7) to AT&T name
pub fn reg_idx_to_name(idx: u8, width: &Width) -> &'static str {
    match idx {
        0 => match width {
            Width::Byte => "al",
            Width::Word => "ax",
            Width::DWord => "eax",
            Width::QWord => "rax",
            Width::Xmm | Width::Ymm | Width::Zmm => {
                format!("xmm{}", idx).as_str()
            }
        },
        1 => match width {
            Width::Byte => "cl",
            Width::Word => "cx",
            Width::DWord => "ecx",
            Width::QWord => "rcx",
            _ => "xmm1",
        },
        // ... more registers
        _ => panic!("Invalid register index: {}", idx),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Width {
    Byte = 8,
    Word = 16,
    DWord = 32,
    QWord = 64,
    Xmm = 128,
    Ymm = 256,
    Zmm = 512,
}

impl Width {
    pub fn bytes(&self) -> u8 {
        match self {
            Width::Byte => 1,
            Width::Word => 2,
            Width::DWord => 4,
            Width::QWord => 8,
            Width::Xmm => 16,
            Width::Ymm => 32,
            Width::Zmm => 64,
        }
    }
}
