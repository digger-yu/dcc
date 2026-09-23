// {isa_name}/registers - Register definitions template
// ISA-specific register constants and utilities

pub const NUM_GPRS: usize = 32;  // Total GP registers
pub const NUM_FPRS: usize = 32;  // Total FP/SIMD registers

pub const STACK_POINTER: u8 = /* register number */;
pub const LINK_REGISTER: u8 = /* register number */;
pub const ZERO_REGISTER: u8 = /* register number */;

/// Get all general purpose registers
pub fn all_gprs() -> &'static [u8] {
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 
      16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31]
}

/// Get argument passing registers (GP)
pub fn arg_regs_gp() -> &'static [u8] {
    &[/* implement based on ABI spec */]
}

/// Get argument passing registers (FP/SIMD)  
pub fn arg_regs_fp() -> &'static [u8] {
    &[/* implement based on ABI spec */]
}

/// Get callee-saved registers
pub fn callee_saved_regs() -> &'static [u8] {
    &[/* implement based on ABI spec */]
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_register_constants() {
        assert_eq!(super::NUM_GPRS, 32);
    }
    
    #[test]
    fn test_arg_reg_count() {
        let arg_regs = super::arg_regs_gp();
        assert_eq!(arg_regs.len(), /* expected count */);
    }
}
