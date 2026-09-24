// src/codegen/x86_64/elf/resolve.rs
// ET_EXEC/ET_REL final symbol resolution and relocation patching.

use crate::codegen::x86_64::{Codegen, PendingCodeRel};
use crate::codegen::shared::elf::R_X86_64_PC32;

impl Codegen {
    pub fn resolve(&mut self, rodata_start: u64, data_start: u64) {
        for (p, name) in &self.pending_data_rels {
            self.disasm_data.insert(*p, name.clone());
        }
        // 修补跳转；ET_REL 下最终仍未解析的符号降级为 PC32 relocation。
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            let Some(&dst) = self.labels.get(&p.target) else {
                if self.relocatable && !p.is_push_imm {
                    let rel_off = p.disp_offset;
                    self.pending_code_rels.push(PendingCodeRel {
                        offset: rel_off,
                        symbol: p.target,
                        rel_type: R_X86_64_PC32,
                        addend: -4,
                    });
                    continue;
                }
                // ET_EXEC 模式下未解析的符号: 发出诊断错误而非 panic.
                // 外部符号 (如 libc 函数) 应由 driver 层的 needs_host_cc_link
                // 委托 host cc 链接; 若到达此处说明绕过了 host cc 回退.
                self.diagnostics.push(crate::diagnostics::Diagnostic {
                    sev: crate::diagnostics::Severity::Error,
                    code: "E1100",
                    msg: format!(
                        "unresolved symbol '{}': ET_EXEC mode cannot \
                         resolve external symbols; use -c for relocatable \
                         output or link with host cc",
                        p.target
                    ),
                    span: crate::diagnostics::Span::point("<link>", 0, 0),
                    hint: Some("compile with -c or use host cc for linking".into()),
                    source_line: None,
                });
                continue;
            };
            let off = p.disp_offset as usize;
            if p.is_push_imm {
                // push $label: imm32 = absolute address (lower 32 bits)
                self.code[off..off + 4].copy_from_slice(&(dst as i32).to_le_bytes());
            } else {
                let rel = (dst as i64) - (p.next_ip as i64);
                let rel_off = p.disp_offset as usize;
                self.code[rel_off..rel_off + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补 RIP-relative 字符串引用 (LEA dst, [RIP+disp32])
        for (code_pos, data) in self.string_refs.drain(..) {
            let str_off = self.string_pool[&data];
            let str_vaddr = rodata_start + str_off;
            let rip = self.entry + code_pos + 7; // LEA is 7 bytes
            let rel = (str_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 3; // disp32 starts at byte 3
            self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 RIP-relative 浮点字面量引用 (movsd/movss xmm0, [RIP+disp32])
        for (code_pos, rodata_off) in self.float_refs.drain(..) {
            let flt_vaddr = rodata_start + rodata_off;
            let rip = self.entry + code_pos + 8; // movsd/movss is 8 bytes
            let rel = (flt_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 4; // disp32 starts at byte 4
            self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 x87 fld [RIP+disp32] 引用 (6 字节指令, disp32 @ byte 2)
        for (code_pos, rodata_off) in self.x87_float_refs.drain(..) {
            let flt_vaddr = rodata_start + rodata_off;
            let rip = self.entry + code_pos + 6; // fld [rip+disp32] is 6 bytes
            let rel = (flt_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 2; // disp32 starts at byte 2
            self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 -fbounds-check ubsan 诊断数据 (LEA rdi, [RIP+disp32] + 指针)
        // ET_EXEC: 镜像在单个 RWX 段, rodata 运行时可写, 直接修补。
        // ET_REL: OutOfBoundsData 在 .data, 由 compile_rel 生成重定位。
        if !self.relocatable {
            for (code_pos, data_off) in self.ubsan_lea_refs.drain(..) {
                let data_vaddr = rodata_start + data_off;
                let rip = self.entry + code_pos + 7; // LEA = 7 bytes
                let rel = (data_vaddr as i64) - (rip as i64);
                let p = code_pos as usize + 3; // disp32 starts at byte 3
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
            for (data_off, field_off, target_off) in self.ubsan_ptr_refs.drain(..) {
                let target_vaddr = rodata_start + target_off;
                // rodata 已拼接进 self.code (exec_text_size 之后), 修补副本
                let fp = (self.exec_text_size + data_off + field_off) as usize;
                self.code[fp..fp + 8].copy_from_slice(&target_vaddr.to_le_bytes());
            }
        }
        // 修补 RIP-relative 全局变量引用 (mov/lea reg, [RIP+disp32])
        for (disp32_pos, var_name) in self.pending_data_rels.drain(..) {
            if let Some(g) = self.globals_raw.get(&var_name) {
                if g.is_tls {
                    continue;
                }
                let var_vaddr = if g.in_data_section {
                    data_start + g.offset
                } else {
                    rodata_start + g.offset
                };
                let rip = self.entry + disp32_pos + 4;
                let rel = (var_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补 SIB base=101 全局访问 (disp32 绝对寻址) - 直接写符号绝对地址
        for (disp32_pos, var_name) in self.pending_sib_data_rels.drain(..) {
            if let Some(g) = self.globals_raw.get(&var_name) {
                let var_vaddr = if g.in_data_section {
                    data_start + g.offset
                } else {
                    rodata_start + g.offset
                };
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(var_vaddr as u32).to_le_bytes());
            }
        }
        // 修补 TLS 变量 RIP-relative 引用（ET_EXEC .tdata 模板）
        let tdata_start = rodata_start + self.rodata.len() as u64;
        for (disp32_pos, var_name) in self.pending_tls_rels.drain(..) {
            // __dcc_tls_base 是合成符号: 指向 TLS 模板起始地址
            if var_name == "__dcc_tls_base" {
                let rip = self.entry + disp32_pos + 4;
                let rel = (tdata_start as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
                continue;
            }
            if let Some(g) = self.globals_raw.get(&var_name) {
                let var_vaddr = tdata_start + g.offset;
                let rip = self.entry + disp32_pos + 4;
                let rel = (var_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补字符串初始化指针 (char *s = "hello"): 指针槽里的 rodata 偏移
        // 替换为绝对地址 (rodata 已拼接到 self.code 的 exec_text_size 之后)。
        for name in &self.rodata_string_init_globals {
            if let Some(g) = self.globals_raw.get(name) {
                let code_off = (self.exec_text_size + g.offset) as usize;
                if code_off + 8 <= self.code.len() {
                    let str_off =
                        u64::from_le_bytes(self.code[code_off..code_off + 8].try_into().unwrap());
                    let abs_addr = rodata_start + str_off;
                    self.code[code_off..code_off + 8].copy_from_slice(&abs_addr.to_le_bytes());
                }
            }
        }
        self.rodata_string_init_globals.clear();
        // 修补 InitList 数组/结构体中的字符串初始化指针槽 (compile_exec 路径):
        // 槽内写入的是 str_off 占位 (emit_string_ptr_slot), 替换为绝对地址。
        for slot_off in std::mem::take(&mut self.rodata_string_slots) {
            let code_off = (self.exec_text_size + slot_off) as usize;
            if code_off + 8 <= self.code.len() {
                let str_off =
                    u64::from_le_bytes(self.code[code_off..code_off + 8].try_into().unwrap());
                let abs_addr = rodata_start + str_off;
                self.code[code_off..code_off + 8].copy_from_slice(&abs_addr.to_le_bytes());
            }
        }
        // 修补 &&label 地址引用 (LEA reg, [RIP+disp32])
        for (disp32_pos, label_name) in self.label_addr_refs.drain(..) {
            if let Some(&label_off) = self.labels.get(&label_name) {
                let label_vaddr = self.entry + label_off;
                let rip = self.entry + disp32_pos + 4;
                let rel = (label_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p + 4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
    }
}
