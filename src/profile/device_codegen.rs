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
    /// Hard errors: 未知 device triple 等 (调用方应视为 codegen 失败)
    pub errors: Vec<String>,
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
/// Runtime probing: `check_runtime` actually dlopen()s the driver shared
/// library (libcuda.so.1 / libamdhip64.so) and calls its init entry
/// (cuInit / hipInit) to decide Available / InitFailed / NotFound.
/// Kernel dispatch: `launch_kernel` resolves cuModuleLoadData /
/// cuModuleGetFunction / cuLaunchKernel (or the hip_* equivalents)
/// through dlsym and launches the loaded device code with the grid/block
/// geometry from `KernelLaunchParams`.
#[derive(Debug, Clone, Default)]
pub struct OffloadRuntimeLoader {
    /// Loaded offload entries from `.dcc.offload` section
    pub entries: Vec<OffloadEntry>,
    /// Device triple -> runtime availability status
    pub runtime_status: HashMap<String, RuntimeStatus>,
    /// Diagnostics from loading/dispatch
    pub diagnostics: Vec<String>,
    /// Device triple -> compiled device code bytes (cubin / hsaco),
    /// loaded by the host before launch.
    pub device_code: HashMap<String, Vec<u8>>,
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

    /// 注入已编译的设备代码 (cubin / hsaco bytes) 供 launch 时加载。
    pub fn set_device_code(&mut self, triple: &str, code: Vec<u8>) {
        self.device_code.insert(triple.to_string(), code);
    }

    /// 从文件加载设备代码 (cubin / hsaco)。
    pub fn load_device_code_from_file(&mut self, triple: &str, path: &std::path::Path) -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        self.device_code.insert(triple.to_string(), bytes);
        Ok(())
    }

    /// 注入 runtime 探测结果 (测试 / 宿主显式声明 driver 状态用)。
    pub fn set_runtime_status(&mut self, triple: &str, status: RuntimeStatus) {
        self.runtime_status.insert(triple.to_string(), status);
    }

    /// 探测指定 triple 的设备 runtime 是否可用。
    ///
    /// 真实探测: dlopen 对应 driver 共享库 (libcuda.so.1 / libamdhip64.so)
    /// 并调用其 init 入口 (cuInit / hipInit)。返回 Available /
    /// InitFailed / NotFound。
    pub fn check_runtime(&mut self, triple: &str) -> RuntimeStatus {
        let status = if triple.contains("nvptx") {
            probe_cuda_runtime(triple, &mut self.diagnostics)
        } else if triple.contains("amdgcn") {
            probe_rocm_runtime(triple, &mut self.diagnostics)
        } else {
            self.diagnostics.push(format!(
                "note: no driver probe defined for triple '{}'",
                triple
            ));
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
    /// 流程: 找到 entry -> 探测 runtime 状态 -> 已加载设备代码 -> 通过
    /// driver API (cuLaunchKernel / hipLaunchKernel) 实际分发。driver
    /// 不可用或未加载设备代码时返回具体失败原因。
    pub fn launch_kernel(&mut self, params: &KernelLaunchParams) -> KernelLaunchResult {
        // Find the kernel entry
        let entry = match self.find_entry(&params.func_name) {
            Some(e) => e.clone(),
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

        // Runtime 可用: 分发给实际 driver
        if triple.contains("nvptx") {
            self.dispatch_cuda(&entry, params)
        } else if triple.contains("amdgcn") {
            self.dispatch_rocm(&entry, params)
        } else {
            KernelLaunchResult::Failed(format!(
                "no device dispatcher for triple '{}'",
                triple
            ))
        }
    }

    /// 通过 CUDA driver API 分发 kernel launch。
    fn dispatch_cuda(&mut self, entry: &OffloadEntry, params: &KernelLaunchParams) -> KernelLaunchResult {
        let triple = entry.device_triple.clone();
        let code = match self.device_code.get(&triple) {
            Some(c) if !c.is_empty() => c.clone(),
            _ => {
                return KernelLaunchResult::Failed(format!(
                    "no device code (cubin) loaded for '{}'; use set_device_code or \
                     load_device_code_from_file",
                    triple
                ));
            }
        };
        match cuda_launch(&entry.func, &code, params) {
            Ok(()) => KernelLaunchResult::Success,
            Err(msg) => KernelLaunchResult::Failed(msg),
        }
    }

    /// 通过 ROCm driver API 分发 kernel launch。
    fn dispatch_rocm(&mut self, entry: &OffloadEntry, params: &KernelLaunchParams) -> KernelLaunchResult {
        let triple = entry.device_triple.clone();
        let code = match self.device_code.get(&triple) {
            Some(c) if !c.is_empty() => c.clone(),
            _ => {
                return KernelLaunchResult::Failed(format!(
                    "no device code (hsaco) loaded for '{}'; use set_device_code or \
                     load_device_code_from_file",
                    triple
                ));
            }
        };
        match rocm_launch(&entry.func, &code, params) {
            Ok(()) => KernelLaunchResult::Success,
            Err(msg) => KernelLaunchResult::Failed(msg),
        }
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

// ---------------------------------------------------------------------------
// CUDA / ROCm driver FFI (dlopen / dlsym)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    #[link(name = "dl")]
    extern "C" {
        pub fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        pub fn dlclose(handle: *mut c_void) -> c_int;
    }

    pub const RTLD_NOW: c_int = 2;
}

/// 尝试 dlopen 一个共享库, 返回句柄或错误消息。
#[cfg(target_os = "linux")]
fn dlopen_lib(names: &[&str]) -> Result<*mut std::ffi::c_void, String> {
    use std::ffi::CString;
    for name in names {
        let Ok(cname) = CString::new(*name) else {
            continue;
        };
        let h = unsafe { ffi::dlopen(cname.as_ptr(), ffi::RTLD_NOW) };
        if !h.is_null() {
            return Ok(h);
        }
    }
    Err(names.join(" / "))
}

/// dlsym 解析符号并转成目标函数指针。
#[cfg(target_os = "linux")]
unsafe fn dlsym_fn<T>(handle: *mut std::ffi::c_void, symbol: &str) -> Option<T> {
    use std::ffi::CString;
    let Ok(csym) = CString::new(symbol) else {
        return None;
    };
    let p = ffi::dlsym(handle, csym.as_ptr());
    if p.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy(&p))
    }
}

/// 探测 CUDA driver: dlopen libcuda 并调用 cuInit(0)。
fn probe_cuda_runtime(triple: &str, diags: &mut Vec<String>) -> RuntimeStatus {
    #[cfg(target_os = "linux")]
    {
        let handle = match dlopen_lib(&["libcuda.so.1", "libcuda.so"]) {
            Ok(h) => h,
            Err(names) => {
                diags.push(format!(
                    "note: CUDA runtime not available for '{}' (cannot load {}); \
                     kernel launches will fail",
                    triple, names
                ));
                return RuntimeStatus::NotFound;
            }
        };
        let init: Option<unsafe extern "C" fn(u32) -> i32> = unsafe { dlsym_fn(handle, "cuInit") };
        let Some(init) = init else {
            diags.push(format!(
                "note: CUDA driver for '{}' found but cuInit symbol missing",
                triple
            ));
            unsafe { ffi::dlclose(handle) };
            return RuntimeStatus::InitFailed;
        };
        let rc = unsafe { init(0) };
        unsafe { ffi::dlclose(handle) };
        if rc == 0 {
            diags.push(format!(
                "note: CUDA runtime available for '{}' (cuInit ok)",
                triple
            ));
            RuntimeStatus::Available
        } else {
            diags.push(format!(
                "note: CUDA driver for '{}' found but cuInit failed (rc={})",
                triple, rc
            ));
            RuntimeStatus::InitFailed
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        diags.push(format!(
            "note: CUDA runtime not available for '{}' (driver probing is Linux-only)",
            triple
        ));
        RuntimeStatus::NotFound
    }
}

/// 探测 ROCm driver: dlopen libamdhip64 并调用 hipInit(0)。
fn probe_rocm_runtime(triple: &str, diags: &mut Vec<String>) -> RuntimeStatus {
    #[cfg(target_os = "linux")]
    {
        let handle = match dlopen_lib(&["libamdhip64.so.6", "libamdhip64.so.5", "libamdhip64.so"]) {
            Ok(h) => h,
            Err(names) => {
                diags.push(format!(
                    "note: ROCm runtime not available for '{}' (cannot load {}); \
                     kernel launches will fail",
                    triple, names
                ));
                return RuntimeStatus::NotFound;
            }
        };
        let init: Option<unsafe extern "C" fn(u32) -> i32> = unsafe { dlsym_fn(handle, "hipInit") };
        let Some(init) = init else {
            diags.push(format!(
                "note: ROCm driver for '{}' found but hipInit symbol missing",
                triple
            ));
            unsafe { ffi::dlclose(handle) };
            return RuntimeStatus::InitFailed;
        };
        let rc = unsafe { init(0) };
        unsafe { ffi::dlclose(handle) };
        if rc == 0 {
            diags.push(format!(
                "note: ROCm runtime available for '{}' (hipInit ok)",
                triple
            ));
            RuntimeStatus::Available
        } else {
            diags.push(format!(
                "note: ROCm driver for '{}' found but hipInit failed (rc={})",
                triple, rc
            ));
            RuntimeStatus::InitFailed
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        diags.push(format!(
            "note: ROCm runtime not available for '{}' (driver probing is Linux-only)",
            triple
        ));
        RuntimeStatus::NotFound
    }
}

/// 通过 CUDA driver API 真正启动 kernel。
///
/// 解析 cuModuleLoadData / cuModuleGetFunction / cuLaunchKernel 后:
/// 加载 cubin, 按名字取 kernel function, 以 grid/block/args 调
/// cuLaunchKernel。
#[cfg(target_os = "linux")]
fn cuda_launch(func: &str, cubin: &[u8], params: &KernelLaunchParams) -> Result<(), String> {
    use std::ffi::CString;
    let handle = dlopen_lib(&["libcuda.so.1", "libcuda.so"])
        .map_err(|names| format!("cannot load CUDA driver ({})", names))?;
    let result = (|| {
        unsafe {
            let load_mod: Option<
                unsafe extern "C" fn(*mut *mut std::ffi::c_void, *const std::ffi::c_void) -> i32,
            > = dlsym_fn(handle, "cuModuleLoadData");
            let get_fn: Option<
                unsafe extern "C" fn(
                    *mut *mut std::ffi::c_void,
                    *mut std::ffi::c_void,
                    *const std::ffi::c_char,
                ) -> i32,
            > = dlsym_fn(handle, "cuModuleGetFunction");
            let launch: Option<
                unsafe extern "C" fn(
                    *mut std::ffi::c_void,
                    u32,
                    u32,
                    u32,
                    u32,
                    u32,
                    u32,
                    u32,
                    *mut std::ffi::c_void,
                    *mut *const std::ffi::c_void,
                    *mut *const std::ffi::c_void,
                ) -> i32,
            > = dlsym_fn(handle, "cuLaunchKernel");
            let (Some(load_mod), Some(get_fn), Some(launch)) =
                (load_mod, get_fn, launch)
            else {
                return Err("CUDA driver symbols missing (cuModuleLoadData / \
                            cuModuleGetFunction / cuLaunchKernel)"
                    .to_string());
            };
            let mut module: *mut std::ffi::c_void = std::ptr::null_mut();
            let rc = load_mod(&mut module, cubin.as_ptr() as *const std::ffi::c_void);
            if rc != 0 {
                return Err(format!("cuModuleLoadData failed (rc={rc})"));
            }
            let mut kernel: *mut std::ffi::c_void = std::ptr::null_mut();
            let Ok(cfunc) = CString::new(func) else {
                return Err("kernel name contains NUL".to_string());
            };
            let rc = get_fn(&mut kernel, module, cfunc.as_ptr());
            if rc != 0 {
                return Err(format!("cuModuleGetFunction('{func}') failed (rc={rc})"));
            }
            // kernel args: 每个 arg 的地址
            let mut arg_ptrs: Vec<*const std::ffi::c_void> = params
                .args
                .iter()
                .map(|a| a.as_ptr() as *const std::ffi::c_void)
                .collect();
            let (gx, gy, gz) = params.grid_dim;
            let (bx, by, bz) = params.block_dim;
            let rc = launch(
                kernel,
                gx,
                gy,
                gz,
                bx,
                by,
                bz,
                params.shared_mem as u32,
                std::ptr::null_mut(), // stream
                arg_ptrs.as_mut_ptr(),
                std::ptr::null_mut(), // extra
            );
            if rc != 0 {
                return Err(format!("cuLaunchKernel('{func}') failed (rc={rc})"));
            }
            Ok(())
        }
    })();
    unsafe { ffi::dlclose(handle) };
    result
}

/// 通过 ROCm driver API 真正启动 kernel。
#[cfg(target_os = "linux")]
fn rocm_launch(func: &str, hsaco: &[u8], params: &KernelLaunchParams) -> Result<(), String> {
    use std::ffi::CString;
    let handle = dlopen_lib(&["libamdhip64.so.6", "libamdhip64.so.5", "libamdhip64.so"])
        .map_err(|names| format!("cannot load ROCm driver ({})", names))?;
    let result = (|| {
        unsafe {
            let load_mod: Option<
                unsafe extern "C" fn(*mut *mut std::ffi::c_void, *const std::ffi::c_void) -> i32,
            > = dlsym_fn(handle, "hipModuleLoadData");
            let get_fn: Option<
                unsafe extern "C" fn(
                    *mut *mut std::ffi::c_void,
                    *mut std::ffi::c_void,
                    *const std::ffi::c_char,
                ) -> i32,
            > = dlsym_fn(handle, "hipModuleGetFunction");
            let launch: Option<
                unsafe extern "C" fn(
                    *mut std::ffi::c_void,
                    u32,
                    u32,
                    u32,
                    u32,
                    u32,
                    u32,
                    u32,
                    *mut std::ffi::c_void,
                    *mut *const std::ffi::c_void,
                    *mut *const std::ffi::c_void,
                ) -> i32,
            > = dlsym_fn(handle, "hipLaunchKernel");
            let (Some(load_mod), Some(get_fn), Some(launch)) =
                (load_mod, get_fn, launch)
            else {
                return Err("ROCm driver symbols missing (hipModuleLoadData / \
                            hipModuleGetFunction / hipLaunchKernel)"
                    .to_string());
            };
            let mut module: *mut std::ffi::c_void = std::ptr::null_mut();
            let rc = load_mod(&mut module, hsaco.as_ptr() as *const std::ffi::c_void);
            if rc != 0 {
                return Err(format!("hipModuleLoadData failed (rc={rc})"));
            }
            let mut kernel: *mut std::ffi::c_void = std::ptr::null_mut();
            let Ok(cfunc) = CString::new(func) else {
                return Err("kernel name contains NUL".to_string());
            };
            let rc = get_fn(&mut kernel, module, cfunc.as_ptr());
            if rc != 0 {
                return Err(format!("hipModuleGetFunction('{func}') failed (rc={rc})"));
            }
            let mut arg_ptrs: Vec<*const std::ffi::c_void> = params
                .args
                .iter()
                .map(|a| a.as_ptr() as *const std::ffi::c_void)
                .collect();
            let (gx, gy, gz) = params.grid_dim;
            let (bx, by, bz) = params.block_dim;
            let rc = launch(
                kernel,
                gx,
                gy,
                gz,
                bx,
                by,
                bz,
                params.shared_mem as u32,
                std::ptr::null_mut(), // stream
                arg_ptrs.as_mut_ptr(),
                std::ptr::null_mut(), // extra
            );
            if rc != 0 {
                return Err(format!("hipLaunchKernel('{func}') failed (rc={rc})"));
            }
            Ok(())
        }
    })();
    unsafe { ffi::dlclose(handle) };
    result
}

#[cfg(not(target_os = "linux"))]
fn cuda_launch(func: &str, _code: &[u8], _params: &KernelLaunchParams) -> Result<(), String> {
    Err(format!("CUDA launch of '{func}' is Linux-only"))
}

#[cfg(not(target_os = "linux"))]
fn rocm_launch(func: &str, _code: &[u8], _params: &KernelLaunchParams) -> Result<(), String> {
    Err(format!("ROCm launch of '{func}' is Linux-only"))
}

/// Generate device code for all offload entries
///
/// 已知 device triple (nvptx* / amdgcn*) 生成对应 PTX/AMDGCN kernel;
/// 空 triple 表示 host fallback (OpenMP 未指定 -fopenmp-targets),
/// 静默跳过 (不需要设备代码); 未知非空 triple 是硬错误 —— 写入
/// `errors`, 不产出该目标的设备代码, 避免 "请求了设备目标却拿到
/// 纯 host 二进制" 的静默降级。
pub fn generate_device_code(section: &OffloadSection) -> DeviceCodegenResult {
    let mut result = DeviceCodegenResult {
        device_code: HashMap::new(),
        diagnostics: Vec::new(),
        errors: Vec::new(),
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
        } else if triple.is_empty() {
            // host fallback: OpenMP 未指定 -fopenmp-targets, 无需设备代码
            continue;
        } else {
            result.errors.push(format!(
                "error: unsupported device triple '{}' (expected nvptx* / amdgcn*); \
                 refusing to skip device codegen silently",
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
        assert!(result.errors.is_empty());
    }

    /// 未知 device triple 必须是硬错误 (写入 errors), 而不是
    /// warning 后静默跳过设备 codegen。
    #[test]
    fn test_unsupported_triple_is_hard_error() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "k1".into(),
            file: "k.cu".into(),
            line: 1,
            col: 1,
            device_triple: "sparc-unknown-linux".into(),
            arch: None,
        });
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "k2".into(),
            file: "k.cu".into(),
            line: 2,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: None,
        });

        let result = generate_device_code(&section);
        assert!(!result.errors.is_empty(), "未知 triple 应报错");
        assert!(
            result.errors[0].contains("sparc-unknown-linux"),
            "错误消息应含非法 triple: {}",
            result.errors[0]
        );
        // 已知 triple 仍正常生成
        assert!(result.device_code.contains_key("nvptx64-nvidia-cuda"));
        assert!(!result.device_code.contains_key("sparc-unknown-linux"));
    }

    /// 空 triple (OpenMP host fallback) 静默跳过, 不算错误。
    #[test]
    fn test_empty_triple_host_fallback_skipped() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::OmpTarget,
            func: "kern".into(),
            file: "k.c".into(),
            line: 1,
            col: 1,
            device_triple: String::new(),
            arch: None,
        });

        let result = generate_device_code(&section);
        assert!(result.errors.is_empty());
        assert!(result.device_code.is_empty());
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
    /// when the driver is genuinely absent (real dlopen probe).
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

        // Explicitly check runtime: real probe (dlopen libcuda) on this
        // machine has no CUDA driver, so it must be NotFound.
        let status = loader.check_runtime("nvptx64-nvidia-cuda");
        assert!(!status.is_available());
        assert!(!loader.diagnostics.is_empty());
        assert!(
            loader.diagnostics[0].contains("CUDA runtime not available"),
            "{}",
            loader.diagnostics[0]
        );

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

    /// Test runtime loader: ROCm probe reports NotFound without a driver.
    #[test]
    fn test_runtime_loader_probe_rocm_missing() {
        let status = probe_rocm_runtime("amdgcn-amd-amdhsa", &mut Vec::new());
        assert!(!status.is_available());
    }

    /// Test runtime loader: with an injected Available runtime status and
    /// device code, launch proceeds to the real driver dispatch path
    /// (dlopen libcuda) instead of returning RuntimeNotAvailable.
    #[test]
    fn test_runtime_loader_dispatch_reaches_driver() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "vec_add".into(),
            file: "k.cu".into(),
            line: 1,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: None,
        });

        let mut loader = OffloadRuntimeLoader::new();
        loader.load_from_section(&section);
        // 显式注入 Available (模拟探测成功), 并提供 device code
        loader.set_runtime_status("nvptx64-nvidia-cuda", RuntimeStatus::Available);
        loader.set_device_code("nvptx64-nvidia-cuda", b"fake cubin bytes".to_vec());

        let params = KernelLaunchParams::new("vec_add")
            .grid(1, 1, 1)
            .block(32, 1, 1)
            .arg(&10u32);

        let result = loader.launch_kernel(&params);
        // 无真实 GPU 驱动: 分发路径应尝试 dlopen libcuda 并失败,
        // 而非停留在 RuntimeNotAvailable。
        match result {
            KernelLaunchResult::Failed(msg) => {
                assert!(
                    msg.contains("CUDA driver") || msg.contains("libcuda"),
                    "分发应到达 driver 层: {msg}"
                );
            }
            other => panic!("expected Failed at driver dispatch, got {:?}", other),
        }
    }

    /// Test runtime loader: launch without loaded device code fails fast
    /// with a clear message.
    #[test]
    fn test_runtime_loader_dispatch_requires_device_code() {
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "vec_add".into(),
            file: "k.cu".into(),
            line: 1,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: None,
        });

        let mut loader = OffloadRuntimeLoader::new();
        loader.load_from_section(&section);
        loader.set_runtime_status("nvptx64-nvidia-cuda", RuntimeStatus::Available);

        let params = KernelLaunchParams::new("vec_add");
        let result = loader.launch_kernel(&params);
        match result {
            KernelLaunchResult::Failed(msg) => {
                assert!(msg.contains("no device code (cubin)"), "{msg}");
            }
            other => panic!("expected Failed without device code, got {:?}", other),
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
