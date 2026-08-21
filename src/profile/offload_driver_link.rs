// src/profile/offload_driver_link.rs - CUDA/ROCm driver linking
//
// Implements actual driver library resolution and link command generation
// for offloaded device code. Given an OffloadConfig + OffloadSection,
// this module:
//   1. Resolves CUDA/ROCm runtime library paths (libcudart.so, libamdhip64.so)
//   2. Generates host linker flags (-L/-l for runtime libraries)
//   3. Generates device link commands (nvcc/clang for device objects)
//   4. Builds fat binary descriptors (host + device objects combined)
//
// Search paths follow standard conventions:
//   CUDA: /usr/local/cuda/lib64, $CUDA_HOME/lib64, ldconfig
//   ROCm: /opt/rocm/lib, $ROCM_PATH/lib, ldconfig

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::offload::{OffloadConfig, OffloadEntry, OffloadKind, OffloadSection};

/// Resolved runtime library for a device backend
#[derive(Debug, Clone)]
pub struct ResolvedRuntime {
    /// Backend kind (CUDA / ROCm)
    pub backend: OffloadBackend,
    /// Path to the runtime shared library (e.g. /usr/local/cuda/lib64/libcudart.so)
    pub library_path: PathBuf,
    /// Library name for -l flag (e.g. "cudart")
    pub link_name: String,
    /// Additional search directories
    pub search_dirs: Vec<PathBuf>,
    /// Whether the library was actually found on disk
    pub found: bool,
}

/// Device backend kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OffloadBackend {
    Cuda,
    Rocm,
}

impl OffloadBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            OffloadBackend::Cuda => "cuda",
            OffloadBackend::Rocm => "rocm",
        }
    }
}

/// Link command for device code
#[derive(Debug, Clone)]
pub struct DeviceLinkCommand {
    /// Backend kind
    pub backend: OffloadBackend,
    /// Compiler/driver to invoke (e.g. "nvcc", "clang++")
    pub driver: String,
    /// Input device objects
    pub inputs: Vec<PathBuf>,
    /// Output path
    pub output: PathBuf,
    /// Additional flags
    pub flags: Vec<String>,
    /// Architecture target (e.g. "sm_80", "gfx906")
    pub arch: String,
}

/// Fat binary descriptor: combines host object + device objects
#[derive(Debug, Clone)]
pub struct FatBinaryDescriptor {
    /// Host object path
    pub host_object: PathBuf,
    /// Device triple -> device object path
    pub device_objects: HashMap<String, PathBuf>,
    /// Embedded runtime library paths
    pub embedded_runtimes: Vec<PathBuf>,
    /// Total size estimate (bytes)
    pub estimated_size: u64,
}

/// Result of driver link resolution
#[derive(Debug, Clone)]
pub struct DriverLinkResult {
    /// Resolved runtimes per backend
    pub runtimes: HashMap<OffloadBackend, ResolvedRuntime>,
    /// Host linker flags (-L / -l)
    pub host_link_flags: Vec<String>,
    /// Device link commands (one per backend + arch)
    pub device_link_commands: Vec<DeviceLinkCommand>,
    /// Fat binary descriptor (if device objects exist)
    pub fat_binary: Option<FatBinaryDescriptor>,
    /// Diagnostics / warnings
    pub diagnostics: Vec<String>,
}

/// CUDA/ROCm driver linker
pub struct OffloadDriverLinker {
    config: OffloadConfig,
    section: OffloadSection,
    /// Extra search paths for runtime libraries
    extra_search_paths: Vec<PathBuf>,
    /// Diagnostics accumulated during resolution
    diagnostics: Vec<String>,
}

impl OffloadDriverLinker {
    pub fn new(config: OffloadConfig, section: OffloadSection) -> Self {
        Self {
            config,
            section,
            extra_search_paths: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Add extra search paths for runtime libraries
    pub fn add_search_path(&mut self, path: PathBuf) {
        self.extra_search_paths.push(path);
    }

    /// Resolve all runtime libraries needed by the offload entries
    pub fn resolve_runtimes(&mut self) -> HashMap<OffloadBackend, ResolvedRuntime> {
        let mut runtimes = HashMap::new();
        let needed_backends = self.needed_backends();

        if needed_backends.contains(&OffloadBackend::Cuda) {
            let rt = self.resolve_cuda_runtime();
            runtimes.insert(OffloadBackend::Cuda, rt);
        }
        if needed_backends.contains(&OffloadBackend::Rocm) {
            let rt = self.resolve_rocm_runtime();
            runtimes.insert(OffloadBackend::Rocm, rt);
        }

        runtimes
    }

    /// Generate complete link result with all flags and commands
    pub fn generate_link_result(&mut self, host_object: Option<&Path>) -> DriverLinkResult {
        let runtimes = self.resolve_runtimes();
        let host_link_flags = self.generate_host_link_flags(&runtimes);
        let device_link_commands = self.generate_device_link_commands();
        let fat_binary = host_object.map(|h| self.build_fat_binary(h, &runtimes));

        DriverLinkResult {
            runtimes,
            host_link_flags,
            device_link_commands,
            fat_binary,
            diagnostics: std::mem::take(&mut self.diagnostics),
        }
    }

    /// Determine which backends are needed based on entries
    fn needed_backends(&self) -> Vec<OffloadBackend> {
        let mut backends = Vec::new();
        let mut has_cuda = false;
        let mut has_rocm = false;

        for entry in &self.section.entries {
            match entry.kind {
                OffloadKind::Cuda => {
                    if !has_cuda {
                        has_cuda = true;
                        backends.push(OffloadBackend::Cuda);
                    }
                }
                OffloadKind::Hip => {
                    if !has_rocm {
                        has_rocm = true;
                        backends.push(OffloadBackend::Rocm);
                    }
                }
                OffloadKind::OmpTarget | OffloadKind::OmpTargetSimd => {
                    // OpenMP target: determine backend from device triple
                    if entry.device_triple.contains("nvptx") && !has_cuda {
                        has_cuda = true;
                        backends.push(OffloadBackend::Cuda);
                    } else if entry.device_triple.contains("amdgcn") && !has_rocm {
                        has_rocm = true;
                        backends.push(OffloadBackend::Rocm);
                    }
                }
            }
        }

        // Also check config flags
        if self.config.cuda && !has_cuda {
            backends.push(OffloadBackend::Cuda);
        }
        if self.config.hip && !has_rocm {
            backends.push(OffloadBackend::Rocm);
        }

        backends
    }

    /// Resolve CUDA runtime library
    fn resolve_cuda_runtime(&mut self) -> ResolvedRuntime {
        let search_dirs = self.cuda_search_dirs();
        let lib_names = ["libcudart.so", "libcudart.so.12", "libcudart.so.11"];

        for dir in &search_dirs {
            for name in &lib_names {
                let path = dir.join(name);
                if path.exists() {
                    self.diagnostics
                        .push(format!("note: found CUDA runtime at {}", path.display()));
                    return ResolvedRuntime {
                        backend: OffloadBackend::Cuda,
                        library_path: path,
                        link_name: "cudart".to_string(),
                        search_dirs: search_dirs.clone(),
                        found: true,
                    };
                }
            }
        }

        // Not found: return with search dirs for diagnostic
        self.diagnostics.push(format!(
            "warning: CUDA runtime not found; searched: {}",
            search_dirs
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));

        ResolvedRuntime {
            backend: OffloadBackend::Cuda,
            library_path: PathBuf::from("libcudart.so"),
            link_name: "cudart".to_string(),
            search_dirs,
            found: false,
        }
    }

    /// Resolve ROCm runtime library
    fn resolve_rocm_runtime(&mut self) -> ResolvedRuntime {
        let search_dirs = self.rocm_search_dirs();
        let lib_names = ["libamdhip64.so", "libamdhip64.so.5", "libamdhip64.so.6"];

        for dir in &search_dirs {
            for name in &lib_names {
                let path = dir.join(name);
                if path.exists() {
                    self.diagnostics
                        .push(format!("note: found ROCm runtime at {}", path.display()));
                    return ResolvedRuntime {
                        backend: OffloadBackend::Rocm,
                        library_path: path,
                        link_name: "amdhip64".to_string(),
                        search_dirs: search_dirs.clone(),
                        found: true,
                    };
                }
            }
        }

        self.diagnostics.push(format!(
            "warning: ROCm runtime not found; searched: {}",
            search_dirs
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));

        ResolvedRuntime {
            backend: OffloadBackend::Rocm,
            library_path: PathBuf::from("libamdhip64.so"),
            link_name: "amdhip64".to_string(),
            search_dirs,
            found: false,
        }
    }

    /// CUDA library search directories
    fn cuda_search_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        // Extra search paths first
        dirs.extend(self.extra_search_paths.iter().cloned());

        // Environment variable
        if let Ok(cuda_home) = std::env::var("CUDA_HOME") {
            dirs.push(PathBuf::from(&cuda_home).join("lib64"));
            dirs.push(PathBuf::from(&cuda_home).join("lib"));
        }

        // Standard paths
        dirs.push(PathBuf::from("/usr/local/cuda/lib64"));
        dirs.push(PathBuf::from("/usr/local/cuda/lib"));
        dirs.push(PathBuf::from("/usr/lib/x86_64-linux-gnu"));
        dirs.push(PathBuf::from("/usr/lib64"));

        dirs
    }

    /// ROCm library search directories
    fn rocm_search_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        // Extra search paths first
        dirs.extend(self.extra_search_paths.iter().cloned());

        // Environment variable
        if let Ok(rocm_path) = std::env::var("ROCM_PATH") {
            dirs.push(PathBuf::from(&rocm_path).join("lib"));
        }

        // Standard paths
        dirs.push(PathBuf::from("/opt/rocm/lib"));
        dirs.push(PathBuf::from("/opt/rocm/lib64"));
        dirs.push(PathBuf::from("/usr/lib/x86_64-linux-gnu"));
        dirs.push(PathBuf::from("/usr/lib64"));

        dirs
    }

    /// Generate host linker flags (-L / -l)
    fn generate_host_link_flags(
        &self,
        runtimes: &HashMap<OffloadBackend, ResolvedRuntime>,
    ) -> Vec<String> {
        let mut flags = Vec::new();

        for (backend, rt) in runtimes {
            // Add search directories
            for dir in &rt.search_dirs {
                flags.push(format!("-L{}", dir.display()));
            }
            // Add library
            flags.push(format!("-l{}", rt.link_name));

            // Backend-specific additional flags
            match backend {
                OffloadBackend::Cuda => {
                    // CUDA needs -lcudadevrt for device runtime
                    flags.push("-lcudadevrt".to_string());
                    // Add rpath for runtime loading: always add from search dirs
                    // so the runtime linker can find the shared library at runtime
                    // even if it wasn't found at compile time.
                    for dir in &rt.search_dirs {
                        flags.push(format!("-Wl,-rpath,{}", dir.display()));
                    }
                    // Also add the actual library path's parent if found
                    if rt.found {
                        if let Some(parent) = rt.library_path.parent() {
                            let parent_str = parent.display().to_string();
                            if !flags
                                .iter()
                                .any(|f| f == &format!("-Wl,-rpath,{}", parent_str))
                            {
                                flags.push(format!("-Wl,-rpath,{}", parent.display()));
                            }
                        }
                    }
                }
                OffloadBackend::Rocm => {
                    // ROCm needs -lamd_comgr for compiler library
                    flags.push("-lamd_comgr".to_string());
                    // Add rpath for runtime loading: always add from search dirs
                    for dir in &rt.search_dirs {
                        flags.push(format!("-Wl,-rpath,{}", dir.display()));
                    }
                    if rt.found {
                        if let Some(parent) = rt.library_path.parent() {
                            let parent_str = parent.display().to_string();
                            if !flags
                                .iter()
                                .any(|f| f == &format!("-Wl,-rpath,{}", parent_str))
                            {
                                flags.push(format!("-Wl,-rpath,{}", parent.display()));
                            }
                        }
                    }
                }
            }
        }

        flags
    }

    /// Generate device link commands
    fn generate_device_link_commands(&self) -> Vec<DeviceLinkCommand> {
        let mut commands = Vec::new();

        // Group entries by (backend, arch)
        let mut groups: HashMap<(OffloadBackend, String), Vec<&OffloadEntry>> = HashMap::new();

        for entry in &self.section.entries {
            let backend = match entry.kind {
                OffloadKind::Cuda => OffloadBackend::Cuda,
                OffloadKind::Hip => OffloadBackend::Rocm,
                OffloadKind::OmpTarget | OffloadKind::OmpTargetSimd => {
                    if entry.device_triple.contains("nvptx") {
                        OffloadBackend::Cuda
                    } else if entry.device_triple.contains("amdgcn") {
                        OffloadBackend::Rocm
                    } else {
                        continue;
                    }
                }
            };

            let arch = entry
                .arch
                .clone()
                .unwrap_or_else(|| default_arch_for_backend(backend));

            groups.entry((backend, arch)).or_default().push(entry);
        }

        for ((backend, arch), entries) in &groups {
            let cmd = match backend {
                OffloadBackend::Cuda => self.generate_nvcc_command(arch, entries),
                OffloadBackend::Rocm => self.generate_rocm_link_command(arch, entries),
            };
            commands.push(cmd);
        }

        commands
    }

    /// Generate nvcc command for CUDA device linking
    fn generate_nvcc_command(&self, arch: &str, entries: &[&OffloadEntry]) -> DeviceLinkCommand {
        let inputs: Vec<PathBuf> = entries
            .iter()
            .map(|e| {
                self.section
                    .device_objects
                    .get(&e.device_triple)
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from(format!("{}.cubin", e.func)))
            })
            .collect();

        let output = PathBuf::from(format!("device_{}.cubin", arch));

        DeviceLinkCommand {
            backend: OffloadBackend::Cuda,
            driver: "nvcc".to_string(),
            inputs,
            output,
            flags: vec![
                "-cubin".to_string(),
                format!("-arch={}", arch),
                "--device-link".to_string(),
            ],
            arch: arch.to_string(),
        }
    }

    /// Generate clang command for ROCm device linking
    fn generate_rocm_link_command(
        &self,
        arch: &str,
        entries: &[&OffloadEntry],
    ) -> DeviceLinkCommand {
        let inputs: Vec<PathBuf> = entries
            .iter()
            .map(|e| {
                self.section
                    .device_objects
                    .get(&e.device_triple)
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from(format!("{}.hsaco", e.func)))
            })
            .collect();

        let output = PathBuf::from(format!("device_{}.hsaco", arch));

        DeviceLinkCommand {
            backend: OffloadBackend::Rocm,
            driver: "clang++".to_string(),
            inputs,
            output,
            flags: vec![
                "--cuda-device-only".to_string(),
                "-emit-llvm".to_string(),
                format!("--offload-arch={}", arch),
            ],
            arch: arch.to_string(),
        }
    }

    /// Build fat binary descriptor
    fn build_fat_binary(
        &self,
        host_object: &Path,
        runtimes: &HashMap<OffloadBackend, ResolvedRuntime>,
    ) -> FatBinaryDescriptor {
        let mut device_objects = HashMap::new();
        let mut embedded_runtimes = Vec::new();

        // Collect device objects from section
        for (triple, path) in &self.section.device_objects {
            device_objects.insert(triple.clone(), path.clone());
        }

        // Collect runtime libraries
        for rt in runtimes.values() {
            if rt.found {
                embedded_runtimes.push(rt.library_path.clone());
            }
        }

        // Estimate size (simplified)
        let estimated_size = estimate_binary_size(host_object, &device_objects, &embedded_runtimes);

        FatBinaryDescriptor {
            host_object: host_object.to_path_buf(),
            device_objects,
            embedded_runtimes,
            estimated_size,
        }
    }

    /// Get accumulated diagnostics
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Render device link commands as shell script
    pub fn render_link_script(&self, result: &DriverLinkResult) -> String {
        let mut script = String::from("#!/bin/bash\n");
        script.push_str("# Generated by dcc offload driver linker\n");
        script.push_str("set -e\n\n");

        // Device link commands
        for cmd in &result.device_link_commands {
            script.push_str(&format!(
                "# {} device link for {}\n",
                cmd.backend.as_str(),
                cmd.arch
            ));
            script.push_str(&cmd.driver);
            for flag in &cmd.flags {
                script.push_str(&format!(" {}", flag));
            }
            for input in &cmd.inputs {
                script.push_str(&format!(" {}", input.display()));
            }
            script.push_str(&format!(" -o {}", cmd.output.display()));
            script.push('\n');
        }

        if !result.device_link_commands.is_empty() {
            script.push('\n');
        }

        // Host link flags
        if !result.host_link_flags.is_empty() {
            script.push_str("# Host link flags\n");
            script.push_str("HOST_LINK_FLAGS=\"");
            script.push_str(&result.host_link_flags.join(" "));
            script.push_str("\"\n");
        }

        script
    }
}

/// Default architecture for a backend when not specified
fn default_arch_for_backend(backend: OffloadBackend) -> String {
    match backend {
        OffloadBackend::Cuda => "sm_50".to_string(),
        OffloadBackend::Rocm => "gfx900".to_string(),
    }
}

/// Estimate fat binary size (simplified heuristic)
fn estimate_binary_size(
    host_object: &Path,
    device_objects: &HashMap<String, PathBuf>,
    runtimes: &[PathBuf],
) -> u64 {
    let mut size: u64 = 0;

    // Host object: try to get actual size, else estimate
    size += host_object.metadata().map(|m| m.len()).unwrap_or(64 * 1024);

    // Device objects: estimate 32KB each
    size += device_objects.len() as u64 * 32 * 1024;

    // Runtimes: estimate 2MB each
    size += runtimes.len() as u64 * 2 * 1024 * 1024;

    // Overhead for fat binary headers
    size += 4096;

    size
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::offload::{OffloadConfig, OffloadEntry, OffloadKind, OffloadSection};

    fn make_cuda_config() -> (OffloadConfig, OffloadSection) {
        let mut config = OffloadConfig {
            cuda: true,
            ..OffloadConfig::default()
        };
        config.target_archs.push("sm_80".to_string());

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

        (config, section)
    }

    fn make_hip_config() -> (OffloadConfig, OffloadSection) {
        let mut config = OffloadConfig {
            hip: true,
            ..OffloadConfig::default()
        };
        config.target_archs.push("gfx906".to_string());

        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Hip,
            func: "mat_mul".into(),
            file: "kernel.hip".into(),
            line: 5,
            col: 1,
            device_triple: "amdgcn-amd-amdhsa".into(),
            arch: Some("gfx906".into()),
        });

        (config, section)
    }

    #[test]
    fn needed_backends_cuda() {
        let (config, section) = make_cuda_config();
        let linker = OffloadDriverLinker::new(config, section);
        let backends = linker.needed_backends();
        assert_eq!(backends, vec![OffloadBackend::Cuda]);
    }

    #[test]
    fn needed_backends_hip() {
        let (config, section) = make_hip_config();
        let linker = OffloadDriverLinker::new(config, section);
        let backends = linker.needed_backends();
        assert_eq!(backends, vec![OffloadBackend::Rocm]);
    }

    #[test]
    fn needed_backends_both() {
        let config = OffloadConfig {
            cuda: true,
            hip: true,
            ..OffloadConfig::default()
        };

        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "k1".into(),
            file: "k.cu".into(),
            line: 1,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: None,
        });
        section.add_entry(OffloadEntry {
            kind: OffloadKind::Hip,
            func: "k2".into(),
            file: "k.hip".into(),
            line: 1,
            col: 1,
            device_triple: "amdgcn-amd-amdhsa".into(),
            arch: None,
        });

        let linker = OffloadDriverLinker::new(config, section);
        let backends = linker.needed_backends();
        assert!(backends.contains(&OffloadBackend::Cuda));
        assert!(backends.contains(&OffloadBackend::Rocm));
    }

    #[test]
    fn resolve_runtimes_returns_entries() {
        let (config, section) = make_cuda_config();
        let mut linker = OffloadDriverLinker::new(config, section);
        let runtimes = linker.resolve_runtimes();
        // CUDA runtime may or may not be found, but entry should exist
        assert!(runtimes.contains_key(&OffloadBackend::Cuda));
        let rt = &runtimes[&OffloadBackend::Cuda];
        assert_eq!(rt.backend, OffloadBackend::Cuda);
        assert_eq!(rt.link_name, "cudart");
    }

    #[test]
    fn host_link_flags_contain_library() {
        let (config, section) = make_cuda_config();
        let mut linker = OffloadDriverLinker::new(config, section);
        let result = linker.generate_link_result(None);
        // Should have -L and -l flags
        assert!(result.host_link_flags.iter().any(|f| f.starts_with("-L")));
        assert!(result.host_link_flags.iter().any(|f| f == "-lcudart"));
    }

    #[test]
    fn device_link_command_for_cuda() {
        let (config, section) = make_cuda_config();
        let mut linker = OffloadDriverLinker::new(config, section);
        let result = linker.generate_link_result(None);
        assert!(!result.device_link_commands.is_empty());
        let cmd = &result.device_link_commands[0];
        assert_eq!(cmd.backend, OffloadBackend::Cuda);
        assert_eq!(cmd.driver, "nvcc");
        assert_eq!(cmd.arch, "sm_80");
        assert!(cmd.flags.contains(&"-cubin".to_string()));
    }

    #[test]
    fn device_link_command_for_rocm() {
        let (config, section) = make_hip_config();
        let mut linker = OffloadDriverLinker::new(config, section);
        let result = linker.generate_link_result(None);
        assert!(!result.device_link_commands.is_empty());
        let cmd = &result.device_link_commands[0];
        assert_eq!(cmd.backend, OffloadBackend::Rocm);
        assert_eq!(cmd.driver, "clang++");
        assert_eq!(cmd.arch, "gfx906");
    }

    #[test]
    fn fat_binary_descriptor_with_host_object() {
        let (config, section) = make_cuda_config();
        let mut linker = OffloadDriverLinker::new(config, section);
        let host = PathBuf::from("/tmp/test_host.o");
        let result = linker.generate_link_result(Some(&host));
        assert!(result.fat_binary.is_some());
        let fb = result.fat_binary.unwrap();
        assert_eq!(fb.host_object, host);
        assert!(fb.estimated_size > 0);
    }

    #[test]
    fn render_link_script_output() {
        let (config, section) = make_cuda_config();
        let mut linker = OffloadDriverLinker::new(config, section);
        let result = linker.generate_link_result(None);
        let script = linker.render_link_script(&result);
        assert!(script.contains("#!/bin/bash"));
        assert!(script.contains("nvcc"));
        assert!(script.contains("sm_80"));
        assert!(script.contains("HOST_LINK_FLAGS"));
    }

    #[test]
    fn search_paths_include_standard_dirs() {
        let (config, section) = make_cuda_config();
        let linker = OffloadDriverLinker::new(config, section);
        let dirs = linker.cuda_search_dirs();
        assert!(dirs
            .iter()
            .any(|p| p == &PathBuf::from("/usr/local/cuda/lib64")));
    }

    #[test]
    fn extra_search_paths_prepended() {
        let (config, section) = make_cuda_config();
        let mut linker = OffloadDriverLinker::new(config, section);
        linker.add_search_path(PathBuf::from("/custom/cuda/lib"));
        let dirs = linker.cuda_search_dirs();
        assert_eq!(dirs[0], PathBuf::from("/custom/cuda/lib"));
    }

    #[test]
    fn diagnostics_populated_on_missing_runtime() {
        let (config, section) = make_cuda_config();
        let mut linker = OffloadDriverLinker::new(config, section);
        let _ = linker.resolve_runtimes();
        // Should have at least one diagnostic (found or not found)
        assert!(!linker.diagnostics().is_empty());
    }

    #[test]
    fn default_arch_for_backend_values() {
        assert_eq!(default_arch_for_backend(OffloadBackend::Cuda), "sm_50");
        assert_eq!(default_arch_for_backend(OffloadBackend::Rocm), "gfx900");
    }

    #[test]
    fn omp_target_with_nvptx_triple_needs_cuda() {
        let config = OffloadConfig::default();
        let mut section = OffloadSection::new();
        section.add_entry(OffloadEntry {
            kind: OffloadKind::OmpTarget,
            func: "omp_kern".into(),
            file: "k.c".into(),
            line: 1,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: None,
        });

        let linker = OffloadDriverLinker::new(config, section);
        let backends = linker.needed_backends();
        assert!(backends.contains(&OffloadBackend::Cuda));
    }
}
