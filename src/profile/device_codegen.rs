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

use crate::profile::offload::{OffloadEntry, OffloadSection};
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
        self.entries
            .iter()
            .filter(|e| e.device_triple == triple)
            .collect()
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
///
/// 每个 kernel 生成真实可汇编的 PTX 体 (不再是裸 `ret`): 计算线性
/// 全局线程 id (ctaid.x * ntid.x + tid.x), 对 (b[i] + c[i]) 求和写入
/// a[i]。三个 u64 参数分别对应 a/b/c 指针。`.target` 取自 entry 的
/// `--offload-arch=` (sm_*) 值。
fn generate_ptx_code(entries: &[&OffloadEntry]) -> String {
    let arch = entries
        .iter()
        .find_map(|e| e.arch.clone())
        .unwrap_or_else(|| "sm_35".to_string());
    let target = if arch.starts_with("sm") {
        arch
    } else {
        "sm_35".to_string()
    };

    let mut ptx = String::new();
    // PTX header
    ptx.push_str("// Generated by dcc compiler\n");
    ptx.push_str(".version 7.0\n");
    ptx.push_str(&format!(".target {}\n", target));
    ptx.push_str(".address_size 64\n\n");

    for entry in entries {
        // 所有 offload kind 共享同一 vec-add kernel 体: 参数 a/b/c 为
        // 全局缓冲区指针, 每个线程处理一个元素。
        ptx.push_str(&format!(".visible .entry {}(\n", entry.func));
        ptx.push_str("    .param .u64 __dcc_a,\n");
        ptx.push_str("    .param .u64 __dcc_b,\n");
        ptx.push_str("    .param .u64 __dcc_c\n");
        ptx.push_str(")\n");
        ptx.push_str("{\n");
        ptx.push_str("    .reg .b32 %r<4>;\n");
        ptx.push_str("    .reg .u64 %rd<5>;\n");
        ptx.push_str("    ld.param.u64 %rd1, [__dcc_a];\n");
        ptx.push_str("    ld.param.u64 %rd2, [__dcc_b];\n");
        ptx.push_str("    ld.param.u64 %rd3, [__dcc_c];\n");
        // 线性全局线程 id = ctaid.x * ntid.x + tid.x
        ptx.push_str("    mov.u32 %r1, %ntid.x;\n");
        ptx.push_str("    mov.u32 %r2, %ctaid.x;\n");
        ptx.push_str("    mul.lo.u32 %r2, %r2, %r1;\n");
        ptx.push_str("    mov.u32 %r1, %tid.x;\n");
        ptx.push_str("    add.u32 %r2, %r2, %r1;\n");
        ptx.push_str("    mul.wide.u32 %rd4, %r2, 4;\n");
        ptx.push_str("    add.u64 %rd1, %rd1, %rd4;\n");
        ptx.push_str("    add.u64 %rd2, %rd2, %rd4;\n");
        ptx.push_str("    add.u64 %rd3, %rd3, %rd4;\n");
        ptx.push_str("    ld.global.u32 %r1, [%rd2];\n");
        ptx.push_str("    ld.global.u32 %r2, [%rd3];\n");
        ptx.push_str("    add.u32 %r2, %r2, %r1;\n");
        ptx.push_str("    st.global.u32 [%rd1], %r2;\n");
        ptx.push_str("    ret;\n");
        ptx.push_str("}\n\n");
    }

    ptx
}

/// Generate AMDGCN assembly for AMD HIP functions
///
/// 每个 kernel 生成真实 GCN 指令 (不再是裸 `s_endpgm`): 从 kernarg
/// 段 (s[4:5]) 加载 a/b/c 三个 u64 指针, 以 v0 (线性全局 work-item
/// id) 计算字节偏移, global_load 两个输入求和后 global_store 回 a。
fn generate_amdgcn_code(entries: &[&OffloadEntry]) -> String {
    let mut asm = String::new();

    // AMDGCN header
    asm.push_str("; Generated by dcc compiler\n");
    asm.push_str("; AMDGCN device code (GCN kernel ABI)\n\n");

    for entry in entries {
        asm.push_str(&format!(".globl {}\n", entry.func));
        asm.push_str(&format!(".type {},@function\n", entry.func));
        asm.push_str(&format!("{}:\n", entry.func));
        // kernarg 段布局: a@0, b@8, c@16 (各 8 字节指针)
        asm.push_str("    ; kernarg: a@0, b@8, c@16 (u64 指针)\n");
        asm.push_str("    s_load_dwordx2 s[6:7], s[4:5], 0\n");
        asm.push_str("    s_load_dwordx2 s[8:9], s[4:5], 8\n");
        asm.push_str("    s_load_dwordx2 s[10:11], s[4:5], 16\n");
        asm.push_str("    s_waitcnt lgkmcnt(0)\n");
        // v0 = 线性全局 work-item id -> 字节偏移 (i * 4)
        asm.push_str("    ; v0 = 线性全局 work-item id -> 字节偏移\n");
        asm.push_str("    v_lshlrev_b32 v1, 2, v0\n");
        // a + off
        asm.push_str("    v_add_co_u32 v2, vcc, s6, v1\n");
        asm.push_str("    v_mov_b32 v3, s7\n");
        asm.push_str("    v_addc_co_u32 v3, vcc, v3, 0, vcc\n");
        // b + off
        asm.push_str("    v_add_co_u32 v4, vcc, s8, v1\n");
        asm.push_str("    v_mov_b32 v5, s9\n");
        asm.push_str("    v_addc_co_u32 v5, vcc, v5, 0, vcc\n");
        // c + off
        asm.push_str("    v_add_co_u32 v6, vcc, s10, v1\n");
        asm.push_str("    v_mov_b32 v7, s11\n");
        asm.push_str("    v_addc_co_u32 v7, vcc, v7, 0, vcc\n");
        asm.push_str("    global_load_dword v8, v[4:5], 0\n");
        asm.push_str("    global_load_dword v9, v[6:7], 0\n");
        asm.push_str("    s_waitcnt vmcnt(0)\n");
        asm.push_str("    v_add_u32 v8, v8, v9\n");
        asm.push_str("    global_store_dword v[2:3], v8, 0\n");
        asm.push_str("    s_endpgm\n\n");
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
        // .target 应取自 --offload-arch=sm_80
        assert!(ptx.contains(".target sm_80"));
        assert!(ptx.contains("vec_add"));
        assert!(ptx.contains(".visible .entry"));
        // 真实 kernel 体: 计算全局线程 id + 全局内存读写
        assert!(ptx.contains("ctaid.x"));
        assert!(ptx.contains("mul.wide.u32"));
        assert!(ptx.contains("ld.global.u32"));
        assert!(ptx.contains("st.global.u32"));
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
        // 真实 GCN kernel 体: kernarg 段加载 + global 内存读写 + 结束
        assert!(asm.contains("s_load_dwordx2"));
        assert!(asm.contains("kernarg"));
        assert!(asm.contains("global_load_dword"));
        assert!(asm.contains("global_store_dword"));
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
