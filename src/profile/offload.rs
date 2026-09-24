// src/profile/offload.rs - Offloading (OpenMP target / CUDA / HIP)
//
// Offloading: 把一段代码 (通常是 `#pragma omp target` / `__global__`)
// 编译成 host + device 两种 object, 运行时由 host 调度到 device.
// dcc 设备 codegen 后端 (PTX / AMDGCN) 在 device_codegen.rs 实现,
// 本模块负责接口、校验与元数据:
//   * TargetTriples: 一组 (kind, triple) 描述 host 和 devices
//   * OffloadSection: 序列化到 .dcc.offload 节 (ELF) 的二进制元数据,
//     供 runtime loader 找到对应 .o / 加载到 device; to_json() 保留
//     作为诊断/debug 输出
//   * 给定 OpenMP/CUDA/HIP 输入, 标记若干函数为 offload entry
//   * CLI 值校验: 非法 triple / arch 报错并带具体值

use std::collections::HashMap;
use std::path::PathBuf;

/// `.dcc.offload` 节二进制格式 magic (8 字节)。
pub const OFFLOAD_MAGIC: &[u8; 8] = b"DCC_OFFL";
/// `.dcc.offload` 节二进制格式版本。
pub const OFFLOAD_FORMAT_VERSION: u8 = 1;

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
    pub fn from_name(s: &str) -> Option<OffloadKind> {
        match s {
            "omp-target" => Some(OffloadKind::OmpTarget),
            "omp-target-simd" => Some(OffloadKind::OmpTargetSimd),
            "cuda" => Some(OffloadKind::Cuda),
            "hip" => Some(OffloadKind::Hip),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OffloadEntry {
    pub kind: OffloadKind,
    pub func: String,
    pub file: String,
    pub line: u32,
    /// 源文件列号 (1-based), 与 line 一起构成精确诊断位置
    pub col: u32,
    /// 设备侧 triple (e.g. "nvptx64-nvidia-cuda", "amdgcn-amd-amdhsa");
    /// 空串表示未指定 (host fallback)
    pub device_triple: String,
    /// `--offload-arch=` 指定的架构 (如 "sm_80" / "gfx906"), 未指定为 None
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct OffloadConfig {
    pub enabled: bool,
    /// `-fopenmp` 或 `-fopenmp=simd` 等
    pub openmp: bool,
    pub openmp_simd: bool,
    /// `-fopenmp=libomp|libgomp|llvm` 选择的 runtime 库名
    pub openmp_library: Option<String>,
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

    /// 校验 `-fopenmp-targets=` 与 `--offload-arch=` 的值。
    /// 返回的 Err 消息带具体非法值, 便于用户定位 CLI 参数问题。
    pub fn validate(&self) -> Result<(), String> {
        for t in &self.target_triples {
            if !is_known_triple(t) {
                return Err(format!(
                    "unknown OpenMP offload target triple '{}' (expected one of: {})",
                    t,
                    known_triples().join(", ")
                ));
            }
        }
        for a in &self.target_archs {
            if arch_to_triple(a).is_none() {
                return Err(format!(
                    "unknown offload arch '{}' (expected sm_<N> / gfx<NNN> / a known host triple)",
                    a
                ));
            }
        }
        Ok(())
    }
}

/// 已知 device / host 侧 triple 集合。
pub fn known_triples() -> [&'static str; 5] {
    [
        "nvptx64-nvidia-cuda",
        "nvptx-nvidia-cuda",
        "amdgcn-amd-amdhsa",
        "x86_64-pc-linux-gnu",
        "aarch64-unknown-linux-gnu",
    ]
}

pub fn is_known_triple(t: &str) -> bool {
    known_triples().contains(&t)
}

/// 把 `--offload-arch=` 的架构名映射为 device triple。
///   * `sm_<N>`        -> nvptx64-nvidia-cuda (NVIDIA)
///   * `gfx<NNN>`      -> amdgcn-amd-amdhsa   (AMD)
///   * host triple     -> 原样 (host fallback, 不做 device codegen)
pub fn arch_to_triple(arch: &str) -> Option<&'static str> {
    if arch.starts_with("sm_") || arch.starts_with("sm") {
        Some("nvptx64-nvidia-cuda")
    } else if arch.starts_with("gfx") {
        Some("amdgcn-amd-amdhsa")
    } else {
        // host triple: 原样接受 (host fallback, 不做 device codegen)
        match arch {
            "nvptx64-nvidia-cuda" => Some("nvptx64-nvidia-cuda"),
            "nvptx-nvidia-cuda" => Some("nvptx-nvidia-cuda"),
            "amdgcn-amd-amdhsa" => Some("amdgcn-amd-amdhsa"),
            "x86_64-pc-linux-gnu" => Some("x86_64-pc-linux-gnu"),
            "aarch64-unknown-linux-gnu" => Some("aarch64-unknown-linux-gnu"),
            _ => None,
        }
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
        // 去重: 同一 (kind, func, file, line, triple, arch) 只保留一条
        if self.entries.iter().any(|x| {
            x.kind == e.kind
                && x.func == e.func
                && x.file == e.file
                && x.line == e.line
                && x.device_triple == e.device_triple
                && x.arch == e.arch
        }) {
            return;
        }
        self.entries.push(e);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 序列化为简化 JSON 形式 (诊断/debug 用, 非 .o 内格式).
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\"entries\":[");
        for (i, e) in self.entries.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"kind\":\"{}\",\"func\":\"{}\",\"file\":\"{}\",\"line\":{},\"col\":{},\"device\":\"{}\"",
                e.kind.as_str(),
                esc_json(&e.func),
                esc_json(&e.file),
                e.line,
                e.col,
                esc_json(&e.device_triple),
            ));
            match &e.arch {
                Some(a) => s.push_str(&format!(",\"arch\":\"{}\"", esc_json(a))),
                None => s.push_str(",\"arch\":null"),
            }
            s.push('}');
        }
        s.push_str("]}");
        s
    }

    /// 序列化为 `.dcc.offload` 节二进制载荷。
    ///
    /// 布局 (小端):
    ///   magic       8B  "DCC_OFFL\0"
    ///   version     u8  1
    ///   n_entries   u16 LE
    ///   entries[]:  kind u8 | func u16+bytes | file u16+bytes |
    ///               line u32 | col u32 | triple u16+bytes |
    ///               has_arch u8 | arch u16+bytes
    pub fn to_binary(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.entries.len() * 48);
        out.extend_from_slice(OFFLOAD_MAGIC);
        out.push(OFFLOAD_FORMAT_VERSION);
        out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        for e in &self.entries {
            match e.kind {
                OffloadKind::OmpTarget => out.push(0),
                OffloadKind::OmpTargetSimd => out.push(1),
                OffloadKind::Cuda => out.push(2),
                OffloadKind::Hip => out.push(3),
            }
            push_str16(&mut out, &e.func);
            push_str16(&mut out, &e.file);
            out.extend_from_slice(&e.line.to_le_bytes());
            out.extend_from_slice(&e.col.to_le_bytes());
            push_str16(&mut out, &e.device_triple);
            match &e.arch {
                Some(a) => {
                    out.push(1);
                    push_str16(&mut out, a);
                },
                None => out.push(0),
            }
        }
        out
    }

    /// 解析 `.dcc.offload` 节二进制载荷 (runtime loader / 测试 roundtrip).
    pub fn from_binary(data: &[u8]) -> Result<OffloadSection, String> {
        if data.len() < 10 || &data[..8] != OFFLOAD_MAGIC {
            return Err("bad .dcc.offload magic".into());
        }
        if data[8] != OFFLOAD_FORMAT_VERSION {
            return Err(format!("unsupported .dcc.offload version {}", data[8]));
        }
        let n = u16::from_le_bytes([data[9], data[10]]) as usize;
        let mut pos = 11usize;
        let mut out = OffloadSection::new();
        for _ in 0..n {
            if pos >= data.len() {
                return Err("truncated .dcc.offload entry header".into());
            }
            let kind = match data[pos] {
                0 => OffloadKind::OmpTarget,
                1 => OffloadKind::OmpTargetSimd,
                2 => OffloadKind::Cuda,
                3 => OffloadKind::Hip,
                k => return Err(format!("unknown .dcc.offload kind {}", k)),
            };
            pos += 1;
            let (func, p) = take_str16(data, pos)?;
            let (file, p) = take_str16(data, p)?;
            if p + 8 > data.len() {
                return Err("truncated .dcc.offload line/col".into());
            }
            let line = u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
            let col = u32::from_le_bytes(data[p + 4..p + 8].try_into().unwrap());
            let (device_triple, p) = take_str16(data, p + 8)?;
            if p >= data.len() {
                return Err("truncated .dcc.offload arch flag".into());
            }
            let has_arch = data[p];
            let (arch, next_pos) = if has_arch != 0 {
                let (a, p) = take_str16(data, p + 1)?;
                (Some(a), p)
            } else {
                (None, p + 1)
            };
            pos = next_pos;
            out.add_entry(OffloadEntry {
                kind,
                func,
                file,
                line,
                col,
                device_triple,
                arch,
            });
        }
        Ok(out)
    }
}

fn push_str16(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn take_str16(data: &[u8], pos: usize) -> Result<(String, usize), String> {
    if pos + 2 > data.len() {
        return Err("truncated .dcc.offload string length".into());
    }
    let len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
    let start = pos + 2;
    if start + len > data.len() {
        return Err("truncated .dcc.offload string body".into());
    }
    let s = std::str::from_utf8(&data[start..start + len])
        .map_err(|_| "non-UTF8 .dcc.offload string".to_string())?
        .to_string();
    Ok((s, start + len))
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

    fn sample_entry() -> OffloadEntry {
        OffloadEntry {
            kind: OffloadKind::Cuda,
            func: "vec_add".into(),
            file: "kernel.cu".into(),
            line: 3,
            col: 1,
            device_triple: "nvptx64-nvidia-cuda".into(),
            arch: Some("sm_80".into()),
        }
    }

    #[test]
    fn offload_section_json_roundtrip() {
        let mut s = OffloadSection::new();
        s.add_entry(sample_entry());
        let j = s.to_json();
        assert!(j.contains("vec_add"));
        assert!(j.contains("nvptx64"));
        assert!(j.contains("\"kind\":\"cuda\""));
        assert!(j.contains("\"col\":1"));
        assert!(j.contains("\"arch\":\"sm_80\""));
    }

    #[test]
    fn offload_section_binary_roundtrip() {
        let mut s = OffloadSection::new();
        s.add_entry(sample_entry());
        s.add_entry(OffloadEntry {
            kind: OffloadKind::OmpTarget,
            func: "kern".into(),
            file: "k.c".into(),
            line: 10,
            col: 3,
            device_triple: "amdgcn-amd-amdhsa".into(),
            arch: None,
        });
        let bin = s.to_binary();
        assert_eq!(&bin[..8], OFFLOAD_MAGIC);
        let back = OffloadSection::from_binary(&bin).expect("parse");
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.entries[0].kind, OffloadKind::Cuda);
        assert_eq!(back.entries[0].func, "vec_add");
        assert_eq!(back.entries[0].col, 1);
        assert_eq!(back.entries[0].arch.as_deref(), Some("sm_80"));
        assert_eq!(back.entries[1].kind, OffloadKind::OmpTarget);
        assert_eq!(back.entries[1].device_triple, "amdgcn-amd-amdhsa");
        assert_eq!(back.entries[1].arch, None);
    }

    #[test]
    fn offload_binary_rejects_bad_magic() {
        let err = OffloadSection::from_binary(b"NOTOFFL\0\x01\x00").unwrap_err();
        assert!(err.contains("magic"), "{err}");
    }

    #[test]
    fn offload_binary_rejects_unsupported_version() {
        let mut s = OffloadSection::new();
        s.add_entry(sample_entry());
        let mut bin = s.to_binary();
        bin[8] = OFFLOAD_FORMAT_VERSION + 1;
        let err = OffloadSection::from_binary(&bin).unwrap_err();
        assert!(err.contains("unsupported .dcc.offload version"), "{err}");
    }

    #[test]
    fn offload_add_entry_dedups() {
        let mut s = OffloadSection::new();
        s.add_entry(sample_entry());
        s.add_entry(sample_entry());
        assert_eq!(s.entries.len(), 1);
    }

    #[test]
    fn arch_to_triple_mapping() {
        assert_eq!(arch_to_triple("sm_80"), Some("nvptx64-nvidia-cuda"));
        assert_eq!(arch_to_triple("sm90"), Some("nvptx64-nvidia-cuda"));
        assert_eq!(arch_to_triple("gfx906"), Some("amdgcn-amd-amdhsa"));
        assert_eq!(arch_to_triple("nvptx64-nvidia-cuda"), Some("nvptx64-nvidia-cuda"));
        assert_eq!(arch_to_triple("sparc"), None);
        assert_eq!(arch_to_triple(""), None);
    }

    #[test]
    fn offload_config_validate_reports_bad_value() {
        let mut cfg = OffloadConfig::default();
        cfg.target_triples.push("mips64-unknown-linux".into());
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("mips64-unknown-linux"), "{err}");
        assert!(err.contains("unknown OpenMP offload target triple"), "{err}");

        let mut cfg2 = OffloadConfig::default();
        cfg2.target_archs.push("sparc".into());
        let err2 = cfg2.validate().unwrap_err();
        assert!(err2.contains("sparc"), "{err2}");
        assert!(err2.contains("unknown offload arch"), "{err2}");

        let mut ok = OffloadConfig::default();
        ok.target_triples.push("nvptx64-nvidia-cuda".into());
        ok.target_archs.push("gfx900".into());
        assert!(ok.validate().is_ok());
    }
}
