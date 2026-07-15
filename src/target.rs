// src/target.rs - 目标平台抽象
// 第一性原理: 后端是"数据驱动"——不同架构共享一套 IR,只换指令编码表。
// suckless 准则: 不为每种架构写一个 codegen.rs;而是 codegen 跑在 target 描述之上。

use crate::cli::{Arch, Os, TargetTriple};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian { Little, Big }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalleeSave { SysV, Win64, AAPCS64, AAPCS }

#[derive(Clone)]
pub struct Target {
    pub triple:     TargetTriple,
    pub arch:       Arch,
    pub ptr_size:   u8,             // 字节,通常 4 或 8
    pub int_size:   u8,             // int 字节
    pub long_size:  u8,             // long 字节
    pub llong_size: u8,             // long long 字节
    pub endian:     Endian,
    pub callee_save: CalleeSave,
    pub is_linux:   bool,
    pub red_zone:   bool,
    pub frame_pointer: bool,
    pub sse: bool,
}

impl Target {
    pub fn resolve(t: &TargetTriple) -> Self {
        let (ptr_size, long_size, int_size, llong_size, endian, callee_save) = match t.arch {
            Arch::X86_64  => (8, 8, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::AArch64 => (8, 8, 4, 8, Endian::Little, CalleeSave::AAPCS64),
            Arch::RiscV64 => (8, 8, 4, 8, Endian::Little, CalleeSave::SysV),
            Arch::Mips64  => (8, 8, 4, 8, Endian::Big,    CalleeSave::AAPCS),
            Arch::Mips    => (4, 4, 4, 8, Endian::Big,    CalleeSave::AAPCS),
            Arch::Arm     => (4, 4, 4, 8, Endian::Little, CalleeSave::AAPCS),
        };
        Self {
            triple: t.clone(),
            arch:   t.arch,
            ptr_size, int_size, long_size, llong_size, endian,
            callee_save,
            is_linux: matches!(t.os, Os::Linux | Os::Freestanding),
            red_zone: true,
            frame_pointer: false,
            sse: true,
        }
    }

    pub fn apply_codegen_options(&mut self, red_zone: bool, frame_pointer: bool, sse: bool) {
        self.red_zone = red_zone;
        self.frame_pointer = frame_pointer;
        self.sse = sse;
    }
}
