// src/profile/device_codegen.rs - Device code generation for offloading
//
// Generates device-side code (PTX for NVIDIA, AMDGCN for AMD) from
// offload entries. Currently generates stub implementations that can
// be linked with the host binary.
//
// Runtime loader: `OffloadRuntimeLoader` reads `.dcc.offload` sections
// from ELF binaries and dispatches kernel launches to device runtimes.
// Currently provides a dispatch interface with diagnostics when device
// runtime is not available (no CUDA/ROCm driver installed).

use crate::profile::offload::{OffloadEntry, OffloadKind, OffloadSection};
use std::collections::HashMap;

/// Device code generation result
#[derive(Debug, Clone)]
pub struct DeviceCodegenResult {
    /// Device triple -> generated code (PTX/AMDGCN assembly)
    pub device_code: HashMap<String, String>,
    /// Diagnostics/warnings
    pub diagnostics: Vec<String>,
}

/// Offload runtime loader: reads `.dcc.offload` sections and dispatches
/// kernel launches to device runtimes.
///
/// Design: Similar to OpenMP target runtime or CUDA driver API.
/// The loader:
///   1. Reads `.dcc.offload` section from ELF binary
///   2. Parses offload entries (kernel name, device triple, arch)
///   3. On kernel launch, looks up device runtime and dispatches
///
/// Current status: Provides dispatch interface with diagnostics when
/// device runtime is not available. Actual CUDA/ROCm driver integration
/// requires linking against libcuda.so / libamdhip64.so at runtime.
#[derive(Debug, Clone, Default)]
pub struct OffloadRuntimeLoader {
    /// Loaded offload entries from `.dcc.offload` section
    pub entries: Vec<OffloadEntry>,
    /// Device triple -> runtime availability status
    pub runtime_status: HashMap<String, RuntimeStatus>,
    /// Diagnostics from loading/dispatch
    pub diagnostics: Vec<String>,
}

/// Runtime availability status for a device triple
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    /// Runtime available and initialized
    Available,
    /// Runtime not found (driver not installed)
    NotFound,
    /// Runtime found but initialization failed
    InitFailed,
    /// Not yet checked
    Unknown,
}

impl RuntimeStatus {
    pub fn is_available(&self) -> bool {
        matches!(self, RuntimeStatus::Available)
    }
}

/// Kernel launch parameters
#[derive(Debug, Clone)]
pub struct KernelLaunchParams {
    /// Kernel function name
    pub func_name: String,
    /// Grid dimensions (blocks)
    pub grid_dim: (u32, u32, u32),
    /// Block dimensions (threads per block)
    pub block_dim: (u32, u32, u32),
    /// Shared memory size in bytes
    pub shared_mem: usize,
    /// Kernel arguments (opaque bytes)
    pub args: Vec<Vec<u8>>,
}

impl KernelLaunchParams {
    pub fn new(func_name: impl Into<String>) -> Self {
        Self {
            func_name: func_name.into(),
            grid_dim: (1, 1, 1),
            block_dim: (1, 1, 1),
            shared_mem: 0,
            args: Vec::new(),
        }
    }

    pub fn grid(mut self, x: u32, y: u32, z: u32) -> Self {
        self.grid_dim = (x, y, z);
        self
    }

    pub fn block(mut self, x: u32, y: u32, z: u32) -> Self {
        self.block_dim = (x, y, z);
        self
    }

    pub fn shared_mem(mut self, bytes: usize) -> Self {
        self.shared_mem = bytes;
        self
    }

    pub fn arg<T: Copy>(mut self, value: &T) -> Self {
        let bytes = unsafe {
            std::slice::from_raw_parts(value as *const T as *const u8, std::mem::size_of::<T>())
        };
        self.args.push(bytes.to_vec());
        self
    }
}

/// Kernel launch result
#[derive(Debug, Clone)]
pub enum KernelLaunchResult {
    /// Launch succeeded
    Success,
    /// Kernel not found in offload entries
    KernelNotFound(String),
    /// Device runtime not available
    RuntimeNotAvailable(String),
    /// Launch failed with error message
    Failed(String),
}

impl OffloadRuntimeLoader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load offload entries from a `.dcc.offload` section binary.
    pub fn load_from_binary(&mut self, data: &[u8]) -> Result<(), String> {
        let section = OffloadSection::from_binary(data)?;
        self.entries = section.entries;
        // Initialize runtime status as Unknown for all device triples
        for entry in &self.entries {
            if !entry.device_triple.is_empty() {
                self.runtime_status
                    .entry(entry.device_triple.clone())
                    .or_insert(RuntimeStatus::Unknown);
            }
        }
        Ok(())
    }

    /// Load offload entries from an OffloadSection directly.
    pub fn load_from_section(&mut self, section: &OffloadSection) {
        self.entries = section.entries.clone();
        for entry in &self.entries {
            if !entry.device_triple.is_empty() {
                self.runtime_status
                    .entry(entry.device_triple.clone())
                    .or_insert(RuntimeStatus::Unknown);
            }
        }
    }

    /// Check if a device runtime is available for the given triple.
    /// This is a stub that always returns NotFound (no actual driver check).
    pub fn check_runtime(&mut self, triple: &str) -> RuntimeStatus {
        let status = if triple.contains("nvptx") {
            // Would check for libcuda.so / CUDA driver
            self.diagnostics.push(format!(
                "note: CUDA runtime not available (libcuda.so not found); \
                 kernel launches to '{}' will fail",
                triple
            ));
            RuntimeStatus::NotFound
        } else if triple.contains("amdgcn") {
            // Would check for libamdhip64.so / ROCm driver
            self.diagnostics.push(format!(
                "note: ROCm runtime not available (libamdhip64.so not found); \
                 kernel launches to '{}' will fail",
                triple
            ));
            RuntimeStatus::NotFound
        } else {
            RuntimeStatus::NotFound
        };
        self.runtime_status.insert(triple.to_string(), status);
        status
    }

    /// Find an offload entry by kernel function name.
    pub fn find_entry(&self, func_name: &str) -> Option<&OffloadEntry> {
        self.entries.iter().find(|e| e.func == func_name)
    }

    /// Launch a kernel on the device.
    ///
    /// Returns KernelLaunchResult indicating success or failure reason.
    /// Currently always returns RuntimeNotAvailable since no actual
    /// device runtime is linked.
    pub fn launch_kernel(&mut self, params: &KernelLaunchParams) -> KernelLaunchResult {
        // Find the kernel entry
        let entry = match self.find_entry(&params.func_name) {
            Some(e) => e,
            None => {
                return KernelLaunchResult::KernelNotFound(params.func_name.clone());
            }
        };

        let triple = entry.device_triple.clone();
        if triple.is_empty() {
            return KernelLaunchResult::Failed(
                "empty device triple (host fallback not implemented)".into(),
            );
        }

        // Check runtime availability
        let status = match self.runtime_status.get(&triple) {
            Some(&s) => s,
            None => self.check_runtime(&triple),
        };

        if !status.is_available() {
            return KernelLaunchResult::RuntimeNotAvailable(triple);
        }

        // Would dispatch to actual device runtime here
        // For now, this path is unreachable since check_runtime always returns NotFound
        KernelLaunchResult::Failed("device dispatch not implemented".into())
    }

    /// Get all registered kernel names.
    pub fn kernel_names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.func.as_str()).collect()
    }

    /// Get entries for a specific device triple.
    pub fn entries_for_triple(&self, triple: &str) -> Vec<&OffloadEntry> {
        self.entries.iter().filter(|e| e.device_triple == triple).collect()
    }
}

/// Generate device code for all offload entries
pub fn generate_device_code(section: &OffloadSection) -> DeviceCodegenResult {
    let mut result = DeviceCodegenResult {
        device_code: HashMap::new(),
        diagnostics: Vec::new(),
    };

    // Group entries by device triple
    let mut by_triple: HashMap<String, Vec<&OffloadEntry>> = HashMap::new();
    for entry in &section.entries {
        by_triple
            .entry(entry.device_triple.clone())
            .or_default()
            .push(entry);
    }

    // Generate code for each device target
    for (triple, entries) in &by_triple {
        let code = if triple.contains("nvptx") {
            generate_ptx_code(entries)
        } else if triple.contains("amdgcn") {
            generate_amdgcn_code(entries)
        } else {
            result.diagnostics.push(format!(
                "warning: unsupported device triple '{}', skipping codegen",
                triple
            ));
            continue;
        };
        result.device_code.insert(triple.clone(), code);
    }

    result
}

/// Generate PTX assembly for NVIDIA CUDA/HIP functions
fn generate_ptx_code(entries: &[&OffloadEntry]) -> String {
    let mut ptx = String::new();
    
    // PTX header
    ptx.push_str("// Generated by dcc compiler\n");
    ptx.push_str(".version 7.0\n");
    ptx.push_str(".target sm_35\n");
    ptx.push_str(".address_size 64\n\n");

    for entry in entries {
        match entry.kind {
            OffloadKind::Cuda | OffloadKind::Hip => {
                // Generate kernel stub
                ptx.push_str(&format!(".visible .entry {}(\n", entry.func));
                ptx.push_str("    .param .u64 __dcc_dummy_param\n");
                ptx.push_str(")\n");
                ptx.push_str("{\n");
                // Stub: just return
                ptx.push_str("    ret;\n");
                ptx.push_str("}\n\n");
            }
            OffloadKind::OmpTarget | OffloadKind::OmpTargetSimd => {
                // OpenMP target regions are more complex, generate stub
                ptx.push_str(&format!(".visible .entry {}(\n", entry.func));
                ptx.push_str("    .param .u64 __dcc_omp_param\n");
                ptx.push_str(")\n");
                ptx.push_str("{\n");
                ptx.push_str("    ret;\n");
                ptx.push_str("}\n\n");
            }
        }
    }

    ptx
}

/// Generate AMDGCN assembly for AMD HIP functions
fn generate_amdgcn_code(entries: &[&OffloadEntry]) -> String {
    let mut asm = String::new();
    
    // AMDGCN header
    asm.push_str("; Generated by dcc compiler\n");
    asm.push_str("; AMDGCN device code\n\n");

    for entry in entries {
        match entry.kind {
            OffloadKind::Cuda | OffloadKind::Hip => {
                // Generate kernel stub
                asm.push_str(&format!(".globl {}\n", entry.func));
                asm.push_str(&format!("{}:\n", entry.func));
                // Stub: just return
                asm.push_str("    s_endpgm\n\n");
            }
            OffloadKind::OmpTarget | OffloadKind::OmpTargetSimd => {
                asm.push_str(&format!(".globl {}\n", entry.func));
                asm.push_str(&format!("{}:\n", entry.func));
                asm.push_str("    s_endpgm\n\n");
            }
        }
    }

    asm
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::offload::{OffloadEntry, OffloadKind, OffloadSection};

    #[test]
    fn test_ptx_generation() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "vec_add".into(),
            file: "kernel.cu".into(),
            line: 10,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: Some("sm_80".into()),
        });

        let result = generate_device_code(&section);
        assert!(result.device_code.contains_key("nvptx64-nvidia-cuda"));
        let ptx = &result.device_code["nvptx64-nvidia-cuda"];
        assert!(ptx.contains(".version 7.0"));
        assert!(ptx.contains(".target sm_35"));
        assert!(ptx.contains("vec_add"));
        assert!(ptx.contains(".visible .entry"));
    }

    #[test]
    fn test_amdgcn_generation() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Hip,
            func: "hip_kernel".into(),
            file: "kernel.hip".into(),
            line: 5,
            col: 1,
            device_triple: "amdgcn-amd-amdhsa".into(),
            arch: Some("gfx906".into()),
        });

        let result = generate_device_code(&section);
        assert!(result.device_code.contains_key("amdgcn-amd-amdhsa"));
        let asm = &result.device_code["amdgcn-amd-amdhsa"];
        assert!(asm.contains("AMDGCN"));
        assert!(asm.contains("hip_kernel"));
        assert!(asm.contains("s_endpgm"));
    }

    #[test]
    fn test_multiple_device_targets() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "kernel1".into(),
            file: "k.cu".into(),
            line: 1,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: None,
        });
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Hip,
            func: "kernel2".into(),
            file: "k.hip".into(),
            line: 2,
            col: 1,
            device_triple: "amdgcn-amd-amdhsa".into(),
            arch: None,
        });

        let result = generate_device_code(&section);
        assert_eq!(result.device_code.len(), 2);
        assert!(result.device_code.contains_key("nvptx64-nvidia-cuda"));
        assert!(result.device_code.contains_key("amdgcn-amd-amdhsa"));
    }

    /// Test runtime loader: load from OffloadSection and query entries
    #[test]
    fn test_runtime_loader_load_from_section() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "vec_add".into(),
            file: "kernel.cu".into(),
            line: 10,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: Some("sm_80".into()),
        });
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Hip,
            func: "mat_mul".into(),
            file: "kernel.hip".into(),
            line: 20,
            col: 1,
            device_triple: "amdgcn-amd-amdhsa".into(),
            arch: Some("gfx906".into()),
        });

        let mut loader = OffloadRuntimeLoader::new();
        loader.load_from_section(&section);

        assert_eq!(loader.entries.len(), 2);
        assert!(loader.find_entry("vec_add").is_some());
        assert!(loader.find_entry("mat_mul").is_some());
        assert!(loader.find_entry("nonexistent").is_none());

        let names = loader.kernel_names();
        assert!(names.contains(&"vec_add"));
        assert!(names.contains(&"mat_mul"));
    }

    /// Test runtime loader: load from binary roundtrip
    #[test]
    fn test_runtime_loader_binary_roundtrip() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "test_kernel".into(),
            file: "test.cu".into(),
            line: 5,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: None,
        });

        let binary = section.to_binary();
        let mut loader = OffloadRuntimeLoader::new();
        loader.load_from_binary(&binary).unwrap();

        assert_eq!(loader.entries.len(), 1);
        assert_eq!(loader.entries[0].func, "test_kernel");
    }

    /// Test runtime loader: kernel launch returns RuntimeNotAvailable
    /// since no actual CUDA/ROCm driver is linked
    #[test]
    fn test_runtime_loader_kernel_launch_not_available() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "my_kernel".into(),
            file: "k.cu".into(),
            line: 1,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: None,
        });

        let mut loader = OffloadRuntimeLoader::new();
        loader.load_from_section(&section);

        // Explicitly check runtime to populate diagnostics
        let status = loader.check_runtime("nvptx64-nvidia-cuda");
        assert!(!status.is_available());
        assert!(!loader.diagnostics.is_empty());
        assert!(loader.diagnostics[0].contains("CUDA runtime not available"));

        let params = KernelLaunchParams::new("my_kernel")
            .grid(256, 1, 1)
            .block(256, 1, 1);

        let result = loader.launch_kernel(&params);
        match result {
            KernelLaunchResult::RuntimeNotAvailable(triple) => {
                assert_eq!(triple, "nvptx64-nvidia-cuda");
            }
            _ => panic!("expected RuntimeNotAvailable, got {:?}", result),
        }
    }

    /// Test runtime loader: kernel not found returns KernelNotFound
    #[test]
    fn test_runtime_loader_kernel_not_found() {
        let mut loader = OffloadRuntimeLoader::new();
        let params = KernelLaunchParams::new("nonexistent_kernel");

        let result = loader.launch_kernel(&params);
        match result {
            KernelLaunchResult::KernelNotFound(name) => {
                assert_eq!(name, "nonexistent_kernel");
            }
            _ => panic!("expected KernelNotFound, got {:?}", result),
        }
    }

    /// Test runtime loader: entries_for_triple filters correctly
    #[test]
    fn test_runtime_loader_entries_for_triple() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "cuda_kernel".into(),
            file: "k.cu".into(),
            line: 1,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: None,
        });
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Hip,
            func: "hip_kernel".into(),
            file: "k.hip".into(),
            line: 1,
            col: 1,
            device_triple: "amdgcn-amd-amdhsa".into(),
            arch: None,
        });

        let mut loader = OffloadRuntimeLoader::new();
        loader.load_from_section(&section);

        let cuda_entries = loader.entries_for_triple("nvptx64-nvidia-cuda");
        assert_eq!(cuda_entries.len(), 1);
        assert_eq!(cuda_entries[0].func, "cuda_kernel");

        let amd_entries = loader.entries_for_triple("amdgcn-amd-amdhsa");
        assert_eq!(amd_entries.len(), 1);
        assert_eq!(amd_entries[0].func, "hip_kernel");

        let unknown_entries = loader.entries_for_triple("unknown-triple");
        assert!(unknown_entries.is_empty());
    }

    /// Test KernelLaunchParams builder
    #[test]
    fn test_kernel_launch_params_builder() {
        let params = KernelLaunchParams::new("test_kernel")
            .grid(4, 4, 1)
            .block(32, 32, 1)
            .shared_mem(1024);

        assert_eq!(params.func_name, "test_kernel");
        assert_eq!(params.grid_dim, (4, 4, 1));
        assert_eq!(params.block_dim, (32, 32, 1));
        assert_eq!(params.shared_mem, 1024);
        assert!(params.args.is_empty());
    }
}
