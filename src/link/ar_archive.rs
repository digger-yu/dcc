// src/link/ar_archive.rs - Unix ar 静态归档解析
//
// 支持 --whole-archive: 从 .a 文件提取所有 .o 成员。
//
// Unix ar 格式:
//   - 魔数: "!<arch>\n" (8 字节)
//   - 每个成员: 60 字节头 + 数据 (2 字节对齐)
//   - 头字段: name/16 date/12 uid/6 gid/6 mode/8 size/10 fmag/2

use std::path::{Path, PathBuf};

/// ar 归档魔数
const AR_MAGIC: &[u8] = b"!<arch>\n";

/// ar 成员头大小
const AR_HDR_SIZE: usize = 60;

/// ar 成员头末尾标志
const AR_FMAG: &[u8] = b"`\n";

/// 从 ar 归档提取所有成员的目标文件数据。
/// 返回 (成员名, 成员数据) 的列表。
pub fn extract_archive_members(archive_data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    if archive_data.len() < 8 {
        return Err("archive too small for magic".into());
    }
    if &archive_data[..8] != AR_MAGIC {
        return Err("not an archive (bad magic)".into());
    }

    let mut members = Vec::new();
    let mut offset = 8; // skip magic
    let mut extended_names: Vec<u8> = Vec::new();

    while offset + AR_HDR_SIZE <= archive_data.len() {
        let hdr = &archive_data[offset..offset + AR_HDR_SIZE];

        // 验证 fmag
        if &hdr[58..60] != AR_FMAG {
            return Err(format!("bad archive member fmag at offset {}", offset));
        }

        // 解析成员大小
        let size_str = std::str::from_utf8(&hdr[48..58])
            .map_err(|e| format!("bad size field: {}", e))?
            .trim();
        let size: usize = size_str
            .parse()
            .map_err(|e| format!("bad size field: {}", e))?;

        // 解析成员名
        let name_raw = std::str::from_utf8(&hdr[..16])
            .map_err(|e| format!("bad name field: {}", e))?
            .trim_end();

        let data_start = offset + AR_HDR_SIZE;
        let data_end = data_start + size;
        if data_end > archive_data.len() {
            return Err(format!(
                "archive member truncated: need {} bytes at offset {}",
                size, data_start
            ));
        }
        let member_data = &archive_data[data_start..data_end];

        // 处理特殊成员
        match name_raw {
            // 符号表 (跳过)
            "/" | "/SYM64/" => {}
            // 扩展名表 (GNU ar 长文件名)
            "//" => {
                extended_names = member_data.to_vec();
            }
            // BSD 风格长文件名
            n if n.starts_with("#1/") => {
                let name_len: usize = n[3..]
                    .parse()
                    .unwrap_or(0);
                if name_len > 0 && member_data.len() > name_len {
                    let name = std::str::from_utf8(&member_data[..name_len])
                        .unwrap_or("<bad-name>")
                        .trim_end_matches('\0')
                        .to_string();
                    let actual_data = member_data[name_len..].to_vec();
                    members.push((name, actual_data));
                }
            }
            // GNU 长文件名引用 "/NNNN"
            n if n.starts_with('/') && n.len() > 1 => {
                if let Ok(name_offset) = n[1..].parse::<usize>() {
                    let name = read_extended_name(&extended_names, name_offset);
                    members.push((name, member_data.to_vec()));
                }
            }
            // 普通成员 "name/"
            n => {
                let name = n.trim_end_matches('/').to_string();
                if !name.is_empty() {
                    members.push((name, member_data.to_vec()));
                }
            }
        }

        // 对齐到 2 字节边界
        offset = data_end + (data_end & 1);
    }

    Ok(members)
}

/// 从扩展名表读取长文件名
fn read_extended_name(extended_names: &[u8], offset: usize) -> String {
    if offset >= extended_names.len() {
        return "<bad-name>".into();
    }
    let slice = &extended_names[offset..];
    let end = slice
        .iter()
        .position(|&b| b == b'/' || b == b'\n')
        .unwrap_or(slice.len());
    std::str::from_utf8(&slice[..end])
        .unwrap_or("<bad-name>")
        .to_string()
}

/// 在搜索路径中查找静态库 .a 文件
pub fn resolve_static_library(
    lib_name: &str,
    search_paths: &[PathBuf],
    sysroot: Option<&Path>,
) -> Option<PathBuf> {
    let filename = format!("lib{}.a", lib_name);

    // 1. 显式搜索路径
    for dir in search_paths {
        let candidate = dir.join(&filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 2. sysroot 前缀
    if let Some(sr) = sysroot {
        let sr_lib_dirs = [
            sr.join("lib"),
            sr.join("lib64"),
            sr.join("usr/lib"),
            sr.join("usr/lib64"),
        ];
        for dir in &sr_lib_dirs {
            let candidate = dir.join(&filename);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 3. 标准系统路径
    let system_dirs = [
        "/usr/lib",
        "/lib",
        "/usr/lib64",
        "/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
    ];
    for dir in &system_dirs {
        let candidate = Path::new(dir).join(&filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

/// 提取 whole-archive 库的所有成员到临时文件。
/// 返回临时文件路径列表 (调用方负责清理)。
pub fn extract_whole_archive_libs(
    whole_archive_libs: &[String],
    search_paths: &[PathBuf],
    sysroot: Option<&Path>,
    verbose: bool,
) -> Result<Vec<PathBuf>, String> {
    let mut extracted = Vec::new();
    let tmp_dir = std::env::temp_dir().join(format!("dcc-wa-{}", std::process::id()));

    for lib_name in whole_archive_libs {
        let archive_path = resolve_static_library(lib_name, search_paths, sysroot)
            .ok_or_else(|| format!("cannot find static library: lib{}.a", lib_name))?;

        let archive_data = std::fs::read(&archive_path)
            .map_err(|e| format!("cannot read {}: {}", archive_path.display(), e))?;

        let members = extract_archive_members(&archive_data)?;

        if verbose {
            eprintln!(
                "dcc-link: --whole-archive: extracted {} members from lib{}.a",
                members.len(),
                lib_name
            );
        }

        // 创建临时目录并写入成员
        let lib_tmp_dir = tmp_dir.join(lib_name);
        std::fs::create_dir_all(&lib_tmp_dir)
            .map_err(|e| format!("cannot create temp dir: {}", e))?;

        for (idx, (name, data)) in members.iter().enumerate() {
            let member_path = lib_tmp_dir.join(format!("{:04}_{}", idx, name));
            std::fs::write(&member_path, data)
                .map_err(|e| format!("cannot write member {}: {}", member_path.display(), e))?;
            extracted.push(member_path);
        }
    }

    Ok(extracted)
}

/// 清理临时提取的归档成员文件
pub fn cleanup_extracted_members(paths: &[PathBuf]) {
    if let Some(first) = paths.first() {
        // 临时文件在 dcc-wa-<pid>/<libname>/ 下
        if let Some(parent) = first.parent() {
            if let Some(grandparent) = parent.parent() {
                let _ = std::fs::remove_dir_all(grandparent);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构建最小 ar 归档用于测试
    fn build_test_archive(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(AR_MAGIC);

        for (name, content) in members {
            // 构建 60 字节头
            let mut hdr = [b' '; AR_HDR_SIZE];
            // name (16 bytes)
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len().min(16);
            hdr[..name_len].copy_from_slice(&name_bytes[..name_len]);
            if name_len < 16 {
                hdr[name_len] = b'/';
            }
            // date (12 bytes) - offset 16
            hdr[16..28].copy_from_slice(b"0           ");
            // uid (6 bytes) - offset 28
            hdr[28..34].copy_from_slice(b"0     ");
            // gid (6 bytes) - offset 34
            hdr[34..40].copy_from_slice(b"0     ");
            // mode (8 bytes) - offset 40
            hdr[40..46].copy_from_slice(b"100644");
            // size (10 bytes) - offset 48
            let size_str = format!("{:<10}", content.len());
            hdr[48..58].copy_from_slice(size_str.as_bytes());
            // fmag (2 bytes) - offset 58
            hdr[58..60].copy_from_slice(AR_FMAG);

            data.extend_from_slice(&hdr);
            data.extend_from_slice(content);
            // 2 字节对齐
            if content.len() & 1 != 0 {
                data.push(b'\n');
            }
        }

        data
    }

    #[test]
    fn extract_empty_archive() {
        let archive = build_test_archive(&[]);
        let members = extract_archive_members(&archive).unwrap();
        assert!(members.is_empty());
    }

    #[test]
    fn extract_single_member() {
        let obj_data = b"\x7fELF fake object data";
        let archive = build_test_archive(&[("foo.o", obj_data)]);
        let members = extract_archive_members(&archive).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].0, "foo.o");
        assert_eq!(members[0].1, obj_data);
    }

    #[test]
    fn extract_multiple_members() {
        let obj1 = b"object one data";
        let obj2 = b"object two data!!"; // 15 bytes, odd
        let obj3 = b"object three";
        let archive = build_test_archive(&[("a.o", obj1), ("b.o", obj2), ("c.o", obj3)]);
        let members = extract_archive_members(&archive).unwrap();
        assert_eq!(members.len(), 3);
        assert_eq!(members[0].0, "a.o");
        assert_eq!(members[0].1, obj1);
        assert_eq!(members[1].0, "b.o");
        assert_eq!(members[1].1, obj2);
        assert_eq!(members[2].0, "c.o");
        assert_eq!(members[2].1, obj3);
    }

    #[test]
    fn bad_magic_rejected() {
        let data = b"not an archive at all!!!";
        assert!(extract_archive_members(data).is_err());
    }

    #[test]
    fn too_small_rejected() {
        let data = b"!<ar";
        assert!(extract_archive_members(data).is_err());
    }

    #[test]
    fn resolve_static_library_not_found() {
        let result = resolve_static_library(
            "nonexistent_lib_xyz_12345",
            &[PathBuf::from("/nonexistent/path")],
            None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn resolve_static_library_found() {
        let dir = std::env::temp_dir().join("dcc-ar-resolve-test");
        let _ = std::fs::create_dir_all(&dir);
        let lib_path = dir.join("libfoo.a");
        std::fs::write(&lib_path, b"fake-archive").unwrap();

        let result = resolve_static_library("foo", &[dir.clone()], None);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), lib_path);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
