// src/profile/offload.rs - Offloading (OpenMP target / CUDA / HIP) 骨架
//
// Offloading: 把一段代码 (通常是 `#pragma omp target` / `__global__`)
// 编译成 host + device 两种 object, 运行时由 host 调度到 device.
// dcc 当前没有完整后端; 但 *接口* 与 *元数据* 必须先行:
//   * TargetTriples: 一组 (kind, triple) 描述 host 和 devices
//   * OffloadSection: 序列化到 .dcc.offload 的元数据, 供 runtime
//     loader 找到对应 .o / 加载到 device
//   * 给定 OpenMP/CUDA/HIP 输入, 标记若干函数为 offload entry.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OffloadKind {
    /// OpenMP target (`#pragma omp target`)
    OmpTarget,
    /// OpenMP target simd
    OmpTargetSimd,
    /// CUDA `__global__` / `__device__`
    Cuda,
    /// HIP `__global__` / `__device__`
    Hip,
}

impl OffloadKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OffloadKind::OmpTarget => "omp-target",
            OffloadKind::OmpTargetSimd => "omp-target-simd",
            OffloadKind::Cuda => "cuda",
            OffloadKind::Hip => "hip",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OffloadEntry {
    pub kind: OffloadKind,
    pub func: String,
    pub file: String,
    pub line: u32,
    /// 设备侧 triple (e.g. "nvptx64-nvidia-cuda", "amdgcn-amd-amdhsa")
    pub device_triple: String,
}

#[derive(Debug, Clone, Default)]
pub struct OffloadConfig {
    pub enabled: bool,
    /// `-fopenmp` 或 `-fopenmp=simd` 等
    pub openmp: bool,
    pub openmp_simd: bool,
    /// `-fcuda` / `-fno-cuda` / `-fhip`
    pub cuda: bool,
    pub hip: bool,
    /// `--offload-arch=...` 列表
    pub target_archs: Vec<String>,
    /// `-fopenmp-targets=...`
    pub target_triples: Vec<String>,
}

impl OffloadConfig {
    pub fn any(&self) -> bool {
        self.openmp || self.cuda || self.hip || !self.target_archs.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub struct OffloadSection {
    pub entries: Vec<OffloadEntry>,
    /// device triple -> 设备 object 路径 (driver 写入)
    pub device_objects: HashMap<String, PathBuf>,
}

impl OffloadSection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_entry(&mut self, e: OffloadEntry) {
        self.entries.push(e);
    }

    /// 序列化为简化 JSON 形式 (后续可换 .dcc.offload 二进制格式).
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\"entries\":[");
        for (i, e) in self.entries.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"kind\":\"{}\",\"func\":\"{}\",\"file\":\"{}\",\"line\":{},\"device\":\"{}\"}}",
                e.kind.as_str(),
                esc_json(&e.func),
                esc_json(&e.file),
                e.line,
                esc_json(&e.device_triple),
            ));
        }
        s.push_str("]}");
        s
    }
}

fn esc_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offload_section_json_roundtrip() {
        let mut s = OffloadSection::new();
        s.add_entry(OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "vec_add".into(),
            file: "kernel.cu".into(),
            line: 3,
            device_triple: "nvptx64-nvidia-cuda".into(),
        });
        let j = s.to_json();
        assert!(j.contains("vec_add"));
        assert!(j.contains("nvptx64"));
        assert!(j.contains("\"kind\":\"cuda\""));
    }
}
