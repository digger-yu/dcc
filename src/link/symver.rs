// src/link/symver.rs - Symbol versioning (.symver) 完整实现
//
// ELF 符号版本化 (`__asm__(".symver name, name@ver")`) 需要:
//   1. .gnu.version 节:  每个符号关联一个版本索引 (16-bit per symbol)
//   2. .gnu.version_d 节: 字符串表, 保存版本定义名 (VERDEF / DT_VERDEF)
//   3. .gnu.version_r 节: 字符串表, 保存版本依赖 (VERNEED / DT_VERNEED)
//
// 本模块实现：
//   - VersionTable: 收集所有 .symver 声明的 (name, version) 对
//   - 检查未定义版本引用 (--no-undefined-version)
//   - 生成 .gnu.version 节数据
//
// 第一性原理：版本化在 ELF 层面只是「给每个 dynsym 加一个 16-bit 索引」。
// 真正难的是建立 name@ver -> 真实符号名 的映射, 以及保证 referenced before defined。

use std::collections::HashMap;

/// 版本定义: 一个 (name, version) 对, 关联到一个基础符号
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionDef {
    /// 真实符号名 (e.g. "memcpy")
    pub base_name: String,
    /// 版本字符串 (e.g. "GLIBC_2.2.5" 或 "" 表示本地/隐藏)
    pub version: String,
    /// 是否隐藏 (foo@@ver 而不是 foo@ver)
    pub hidden: bool,
}

/// 版本依赖: 引用了某 SONAME 的某版本
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionNeed {
    pub soname: String,
    pub version: String,
}

/// 完整版本表
#[derive(Debug, Default, Clone)]
pub struct VersionTable {
    /// 解析到的所有 .symver 声明
    pub defs: Vec<VersionDef>,
    /// 引用的外部版本 (DT_VERNEED)
    pub needs: Vec<VersionNeed>,
    /// name@ver → 真实符号名 的快速查询表
    pub symver_lookup: HashMap<String, String>,
    /// name@@ver → 真实符号名 (hidden 版本)
    pub hidden_lookup: HashMap<String, String>,
    /// 解析时收集的所有被引用版本字符串 (用于 .gnu.version_d)
    pub version_strings: Vec<String>,
    /// 解析时收集的 .symver 错误
    pub errors: Vec<String>,
}

impl VersionTable {
    pub fn new() -> Self { Self::default() }

    /// 添加一个 .symver 声明
    /// 形如: ".symver memcpy, memcpy@GLIBC_2.14"
    pub fn add_symver(&mut self, asm_directive: &str) {
        // 解析 ".symver NAME, NAME@VER" 或 ".symver NAME, NAME@@VER" (hidden)
        let s = asm_directive.trim().trim_start_matches(".symver").trim();
        if s.is_empty() {
            self.errors.push("empty .symver directive".into());
            return;
        }
        // 拆分逗号
        let parts: Vec<&str> = s.splitn(2, ',').map(|x| x.trim()).collect();
        if parts.len() != 2 {
            self.errors.push(format!("malformed .symver: {}", asm_directive));
            return;
        }
        let base = parts[0];
        let target = parts[1];
        // 解析 target: NAME@VER 或 NAME@@VER
        let (alias_name, ver_str, hidden) = if let Some(idx) = target.rfind('@') {
            let before = &target[..idx];
            let after = &target[idx + 1..];
            if before.ends_with('@') {
                // NAME@@VER
                (before.strip_suffix('@').unwrap().to_string(), after.to_string(), true)
            } else {
                (before.to_string(), after.to_string(), false)
            }
        } else {
            self.errors.push(format!("missing @ in .symver target: {}", target));
            return;
        };
        let def = VersionDef {
            base_name: base.to_string(),
            version: ver_str.clone(),
            hidden,
        };
        let key = if hidden {
            format!("{}@@{}", alias_name, ver_str)
        } else {
            format!("{}@{}", alias_name, ver_str)
        };
        if hidden {
            self.hidden_lookup.insert(key, base.to_string());
        } else {
            self.symver_lookup.insert(key, base.to_string());
        }
        // 收集唯一版本字符串
        if !ver_str.is_empty() && !self.version_strings.contains(&ver_str) {
            self.version_strings.push(ver_str.clone());
        }
        self.defs.push(def);
    }

    /// 检测未定义版本引用 (--no-undefined-version)
    /// 应当传所有未定义符号 (ref_name)
    pub fn check_undef(&self, refs: &[String]) -> Vec<String> {
        let mut errs = Vec::new();
        for r in refs {
            if r.contains('@') {
                // name@VER 形式但未在 symver_lookup 中找到
                if !self.symver_lookup.contains_key(r) && !self.hidden_lookup.contains_key(r) {
                    errs.push(format!("undefined versioned reference: {}", r));
                }
            }
        }
        errs
    }

    /// 查找 name@ver 形式的引用对应的真实符号
    pub fn resolve(&self, refname: &str) -> Option<&str> {
        if let Some(v) = self.symver_lookup.get(refname) {
            return Some(v.as_str());
        }
        if let Some(v) = self.hidden_lookup.get(refname) {
            return Some(v.as_str());
        }
        None
    }

    /// 生成 .gnu.version 节数据 (每个 dynsym 一个 16-bit 版本索引)
    /// 输入: dynsym 数量, dynsym 名字列表
    /// 输出: (data, version_index_for_each_sym)
    pub fn gen_gnu_version(&self, dynsym_names: &[String]) -> (Vec<u8>, Vec<u16>) {
        // 0 = VER_NDX_LOCAL
        // 1+ = 在 .gnu.version_d 中的索引 (0-based, 1-based after the implicit empty)
        // 这里采用简化: 1 = unversioned
        let mut version_str_to_idx: HashMap<String, u16> = HashMap::new();
        // 0 = local, 1 = global unversioned, 2..N = versioned
        for vs in &self.version_strings {
            if !version_str_to_idx.contains_key(vs) {
                let next = (version_str_to_idx.len() as u16) + 2;
                version_str_to_idx.insert(vs.clone(), next);
            }
        }
        let mut data = Vec::with_capacity(dynsym_names.len() * 2);
        let mut indices = Vec::with_capacity(dynsym_names.len());
        for name in dynsym_names {
            let idx = if name.is_empty() {
                0u16  // VER_NDX_LOCAL
            } else if let Some(ver) = self.find_version_for(name) {
                *version_str_to_idx.get(&ver).unwrap_or(&1)
            } else {
                1u16  // VER_NDX_GLOBAL (unversioned)
            };
            data.extend_from_slice(&idx.to_le_bytes());
            indices.push(idx);
        }
        (data, indices)
    }

    /// 给定一个真实符号名, 找出它对应的版本 (优先用 .symver 表)
    fn find_version_for(&self, real_name: &str) -> Option<String> {
        for d in &self.defs {
            if d.base_name == real_name {
                return Some(d.version.clone());
            }
        }
        None
    }

    /// 生成 .gnu.version_d (DT_VERDEF) 节数据
    /// 格式: Verdef { vd_version, vd_flags, vd_ndx, vd_cnt, vd_hash, vd_aux, vd_next }
    ///       + Verdaux { vda_name, vda_next }
    ///       + 字符串表 (按 vd_aux 偏移索引)
    pub fn gen_verdef(&self) -> Vec<u8> {
        // 简化: 写一个 Verdef, 包含所有版本字符串
        let mut strtab = vec![0u8];
        let mut verdaux_data = Vec::new();
        let mut count = 0u16;
        for vs in &self.version_strings {
            let name_off = strtab.len() as u32;
            // Verdaux: vda_name (4) + vda_next (4)
            verdaux_data.extend_from_slice(&name_off.to_le_bytes());
            verdaux_data.extend_from_slice(&0u32.to_le_bytes());
            for b in vs.bytes() { strtab.push(b); }
            strtab.push(0);
            count += 1;
        }
        // 单个 Verdef
        let mut verdef = Vec::new();
        verdef.extend_from_slice(&1u16.to_le_bytes());  // vd_version
        verdef.extend_from_slice(&0u16.to_le_bytes());  // vd_flags
        verdef.extend_from_slice(&1u16.to_le_bytes());  // vd_ndx (第一个 = 1)
        verdef.extend_from_slice(&count.to_le_bytes());  // vd_cnt
        verdef.extend_from_slice(&0x551u32.to_le_bytes());  // vd_hash (Elf32_Vernaux 简化)
        verdef.extend_from_slice(&16u16.to_le_bytes());  // vd_aux (verdef 之后偏移)
        verdef.extend_from_slice(&0u32.to_le_bytes());  // vd_next (0 = 终止)
        verdef.extend_from_slice(&verdaux_data);
        verdef.extend_from_slice(&strtab);
        verdef
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_symver_simple() {
        let mut t = VersionTable::new();
        t.add_symver(".symver memcpy,memcpy@GLIBC_2.14");
        assert_eq!(t.defs.len(), 1);
        assert_eq!(t.defs[0].base_name, "memcpy");
        assert_eq!(t.defs[0].version, "GLIBC_2.14");
        assert!(!t.defs[0].hidden);
    }

    #[test]
    fn parse_symver_hidden() {
        let mut t = VersionTable::new();
        t.add_symver(".symver foo,foo@@MYLIB_1.0");
        assert!(t.defs[0].hidden);
    }

    #[test]
    fn lookup_works() {
        let mut t = VersionTable::new();
        t.add_symver(".symver memcpy,memcpy@GLIBC_2.14");
        assert_eq!(t.resolve("memcpy@GLIBC_2.14"), Some("memcpy"));
    }

    #[test]
    fn no_undefined_version_check() {
        let mut t = VersionTable::new();
        // 只声明了 GLIBC_2.14, 引用 2.17 应当报错
        t.add_symver(".symver memcpy,memcpy@GLIBC_2.14");
        let errs = t.check_undef(&["memcpy@GLIBC_2.17".to_string()]);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("undefined"));
    }

    #[test]
    fn no_undefined_version_passes() {
        let mut t = VersionTable::new();
        t.add_symver(".symver memcpy,memcpy@GLIBC_2.14");
        let errs = t.check_undef(&["memcpy@GLIBC_2.14".to_string()]);
        assert!(errs.is_empty());
    }

    #[test]
    fn gen_gnu_version_layout() {
        let mut t = VersionTable::new();
        t.add_symver(".symver foo,foo@V1");
        let (data, idx) = t.gen_gnu_version(&["".into(), "foo".into()]);
        assert_eq!(data.len(), 4);
        assert_eq!(idx[0], 0);  // empty -> local
        assert!(idx[1] >= 2);   // foo@V1 -> some non-zero index
    }

    #[test]
    fn gen_verdef_layout() {
        let mut t = VersionTable::new();
        t.add_symver(".symver foo,foo@V1");
        t.add_symver(".symver bar,bar@V2");
        let v = t.gen_verdef();
        // 至少 16 字节 Verdef 头 + 16*N Verdaux + strtab
        assert!(v.len() >= 16);
    }
}
