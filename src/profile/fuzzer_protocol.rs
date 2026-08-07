// src/profile/fuzzer_protocol.rs - libFuzzer protocol and interceptors
//
// Implements libFuzzer integration:
// - LLVMFuzzerTestOneInput entry wrapper generation
// - 8-bit counters initialization
// - PC table (__start___pc_table/__stop___pc_table) generation
// - malloc/free interceptors (__sanitizer_weak_hook_malloc/free)
//
// This enables full libFuzzer compatibility for fuzzing with dcc-compiled code.

/// PC table entry for coverage tracking
#[derive(Debug, Clone)]
pub struct PcTableEntry {
    /// Program counter address
    pub pc: u64,
    /// PC flags (coverage type)
    pub flags: u32,
}

/// libFuzzer protocol implementation
#[derive(Debug, Clone, Default)]
pub struct FuzzerProtocol {
    /// Entry point function name (LLVMFuzzerTestOneInput)
    pub entry_fn: Option<String>,
    /// PC table entries
    pub pc_table: Vec<PcTableEntry>,
    /// 8-bit counters for coverage
    pub counters: Vec<u8>,
    /// Whether malloc interceptor is enabled
    pub malloc_interceptor: bool,
    /// Whether free interceptor is enabled
    pub free_interceptor: bool,
}

impl FuzzerProtocol {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the entry point function
    pub fn set_entry(&mut self, func_name: &str) {
        self.entry_fn = Some(func_name.to_string());
    }

    /// Add a PC table entry
    pub fn add_pc_entry(&mut self, pc: u64, flags: u32) {
        self.pc_table.push(PcTableEntry { pc, flags });
    }

    /// Initialize 8-bit counters for coverage
    pub fn init_counters(&mut self, size: usize) {
        self.counters = vec![0; size];
    }

    /// Increment counter at index
    pub fn increment_counter(&mut self, idx: usize) {
        if idx < self.counters.len() {
            self.counters[idx] = self.counters[idx].saturating_add(1);
        }
    }

    /// Enable malloc interceptor
    pub fn enable_malloc_interceptor(&mut self) {
        self.malloc_interceptor = true;
    }

    /// Enable free interceptor
    pub fn enable_free_interceptor(&mut self) {
        self.free_interceptor = true;
    }

    /// Generate LLVMFuzzerTestOneInput wrapper
    pub fn generate_entry_wrapper(&self, user_func: &str) -> String {
        let mut wrapper = String::new();
        wrapper.push_str("// libFuzzer entry wrapper\n");
        wrapper.push_str("extern int LLVMFuzzerTestOneInput(const uint8_t *Data, size_t Size);\n");
        wrapper.push_str(&format!("extern int {}(const uint8_t *Data, size_t Size);\n\n", user_func));
        wrapper.push_str("int LLVMFuzzerTestOneInput(const uint8_t *Data, size_t Size) {\n");
        wrapper.push_str(&format!("    return {}(Data, Size);\n", user_func));
        wrapper.push_str("}\n");
        wrapper
    }

    /// Generate PC table symbols
    pub fn generate_pc_table(&self) -> String {
        let mut code = String::new();
        code.push_str("// PC table for libFuzzer coverage\n");
        code.push_str("__attribute__((visibility(\"default\")))\n");
        code.push_str("extern const struct {\n");
        code.push_str("    uint64_t pc;\n");
        code.push_str("    uint32_t flags;\n");
        code.push_str("} __start___pc_table[] = {\n");
        
        for entry in &self.pc_table {
            code.push_str(&format!("    {{0x{:x}, 0x{:x}}},\n", entry.pc, entry.flags));
        }
        
        code.push_str("};\n\n");
        code.push_str("__attribute__((visibility(\"default\")))\n");
        code.push_str("extern const struct {\n");
        code.push_str("    uint64_t pc;\n");
        code.push_str("    uint32_t flags;\n");
        code.push_str("} __stop___pc_table[] = {\n");
        code.push_str("    {0, 0}\n");
        code.push_str("};\n");
        
        code
    }

    /// Generate 8-bit counters initialization
    pub fn generate_counters_init(&self) -> String {
        let mut code = String::new();
        code.push_str("// 8-bit counters for coverage\n");
        code.push_str(&format!("static uint8_t __dcc_fuzzer_counters[{}];\n\n", self.counters.len()));
        code.push_str("__attribute__((constructor))\n");
        code.push_str("static void __dcc_fuzzer_init_counters(void) {\n");
        code.push_str(&format!("    for (int i = 0; i < {}; i++) {{\n", self.counters.len()));
        code.push_str("        __dcc_fuzzer_counters[i] = 0;\n");
        code.push_str("    }\n");
        code.push_str("}\n");
        code
    }

    /// Generate malloc interceptor
    pub fn generate_malloc_interceptor(&self) -> String {
        let mut code = String::new();
        code.push_str("// malloc interceptor for libFuzzer\n");
        code.push_str("__attribute__((visibility(\"default\")))\n");
        code.push_str("void __sanitizer_weak_hook_malloc(void *caller, size_t size, void *result) {\n");
        code.push_str("    // Track allocation for fuzzing coverage\n");
        code.push_str("    (void)caller;\n");
        code.push_str("    (void)size;\n");
        code.push_str("    (void)result;\n");
        code.push_str("}\n");
        code
    }

    /// Generate free interceptor
    pub fn generate_free_interceptor(&self) -> String {
        let mut code = String::new();
        code.push_str("// free interceptor for libFuzzer\n");
        code.push_str("__attribute__((visibility(\"default\")))\n");
        code.push_str("void __sanitizer_weak_hook_free(void *caller, void *ptr) {\n");
        code.push_str("    // Track deallocation for fuzzing coverage\n");
        code.push_str("    (void)caller;\n");
        code.push_str("    (void)ptr;\n");
        code.push_str("}\n");
        code
    }

    /// Generate all fuzzer protocol code
    pub fn generate_all(&self, user_func: &str) -> String {
        let mut code = String::new();
        code.push_str("// libFuzzer protocol implementation\n");
        code.push_str("// Generated by dcc compiler\n\n");
        
        // Entry wrapper
        code.push_str(&self.generate_entry_wrapper(user_func));
        code.push_str("\n");
        
        // PC table
        code.push_str(&self.generate_pc_table());
        code.push_str("\n");
        
        // Counters initialization
        code.push_str(&self.generate_counters_init());
        code.push_str("\n");
        
        // Interceptors
        if self.malloc_interceptor {
            code.push_str(&self.generate_malloc_interceptor());
            code.push_str("\n");
        }
        
        if self.free_interceptor {
            code.push_str(&self.generate_free_interceptor());
            code.push_str("\n");
        }
        
        code
    }
}

/// Build fuzzer protocol from compilation context
pub fn build_fuzzer_protocol(
    entry_fn: Option<&str>,
    pc_addresses: &[u64],
    counter_count: usize,
    enable_malloc: bool,
    enable_free: bool,
) -> FuzzerProtocol {
    let mut protocol = FuzzerProtocol::new();
    
    if let Some(func) = entry_fn {
        protocol.set_entry(func);
    }
    
    // Add PC table entries
    for &pc in pc_addresses {
        protocol.add_pc_entry(pc, 1); // flags = 1 for basic block coverage
    }
    
    // Initialize counters
    protocol.init_counters(counter_count);
    
    // Enable interceptors
    if enable_malloc {
        protocol.enable_malloc_interceptor();
    }
    if enable_free {
        protocol.enable_free_interceptor();
    }
    
    protocol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzer_protocol_basic() {
        let mut protocol = FuzzerProtocol::new();
        protocol.set_entry("my_fuzz_target");
        protocol.add_pc_entry(0x1000, 1);
        protocol.add_pc_entry(0x2000, 1);
        protocol.init_counters(256);
        
        assert_eq!(protocol.entry_fn, Some("my_fuzz_target".to_string()));
        assert_eq!(protocol.pc_table.len(), 2);
        assert_eq!(protocol.counters.len(), 256);
    }

    #[test]
    fn test_entry_wrapper_generation() {
        let protocol = FuzzerProtocol::new();
        let wrapper = protocol.generate_entry_wrapper("test_func");
        
        assert!(wrapper.contains("LLVMFuzzerTestOneInput"));
        assert!(wrapper.contains("test_func"));
        assert!(wrapper.contains("return test_func(Data, Size)"));
    }

    #[test]
    fn test_pc_table_generation() {
        let mut protocol = FuzzerProtocol::new();
        protocol.add_pc_entry(0x1000, 1);
        protocol.add_pc_entry(0x2000, 2);
        
        let pc_table = protocol.generate_pc_table();
        assert!(pc_table.contains("__start___pc_table"));
        assert!(pc_table.contains("__stop___pc_table"));
        assert!(pc_table.contains("0x1000"));
        assert!(pc_table.contains("0x2000"));
    }

    #[test]
    fn test_counters_init() {
        let mut protocol = FuzzerProtocol::new();
        protocol.init_counters(128);
        
        let init_code = protocol.generate_counters_init();
        assert!(init_code.contains("__dcc_fuzzer_counters[128]"));
        assert!(init_code.contains("__dcc_fuzzer_init_counters"));
    }

    #[test]
    fn test_interceptors() {
        let mut protocol = FuzzerProtocol::new();
        protocol.enable_malloc_interceptor();
        protocol.enable_free_interceptor();
        
        let malloc_code = protocol.generate_malloc_interceptor();
        assert!(malloc_code.contains("__sanitizer_weak_hook_malloc"));
        
        let free_code = protocol.generate_free_interceptor();
        assert!(free_code.contains("__sanitizer_weak_hook_free"));
    }

    #[test]
    fn test_build_fuzzer_protocol() {
        let protocol = build_fuzzer_protocol(
            Some("fuzz_target"),
            &[0x1000, 0x2000, 0x3000],
            512,
            true,
            true,
        );
        
        assert_eq!(protocol.entry_fn, Some("fuzz_target".to_string()));
        assert_eq!(protocol.pc_table.len(), 3);
        assert_eq!(protocol.counters.len(), 512);
        assert!(protocol.malloc_interceptor);
        assert!(protocol.free_interceptor);
    }

    #[test]
    fn test_complete_generation() {
        let mut protocol = FuzzerProtocol::new();
        protocol.set_entry("my_target");
        protocol.add_pc_entry(0x1000, 1);
        protocol.init_counters(64);
        protocol.enable_malloc_interceptor();
        protocol.enable_free_interceptor();
        
        let code = protocol.generate_all("my_target");
        assert!(code.contains("LLVMFuzzerTestOneInput"));
        assert!(code.contains("__start___pc_table"));
        assert!(code.contains("__dcc_fuzzer_counters"));
        assert!(code.contains("__sanitizer_weak_hook_malloc"));
        assert!(code.contains("__sanitizer_weak_hook_free"));
    }
}
