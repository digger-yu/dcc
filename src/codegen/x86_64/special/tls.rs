// src/codegen/x86_64/special/tls.rs
// Thread-Local Storage (TLS) 的 x86_64 codegen 独立 helper。
//
// 本文件由 mod.rs 逐字搬迁而来 (batch E)：`emit_tls_fs_setup`（ET_EXEC 下用
// arch_prctl(ARCH_SET_FS) 把 FS 基址指向 TLS 模板）原样剪切进 `impl Codegen`
// 块，函数体一字未改，仅把 `fn` 提为 `pub fn`。
//
// 说明：mod.rs 中 TLS 的其余逻辑（`pending_tls_rels` 的登记与 resolve() 修补、
// `--tls-model` 属性 → 重定位类型选择）都内联在 emit_global_addr / resolve /
// compile_rel 等大函数体内，按"不散落在大函数内部的代码段不要动"的纪律未搬迁。
// 原影子 `TlsModel` 枚举与 `emit_tls_address_calculation`（生产代码中不存在该
// 抽象，TLS 模型由 tls_model 字符串 + 重定位类型直接决定，且函数体全是
// unimplemented!()）按"以生产代码为准"作废删除。

use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::insn::Insn;

impl Codegen {
    /// ET_EXEC：设置 FS 基址指向 TLS 模板（可写 PT_LOAD 段内）
    pub fn emit_tls_fs_setup(&mut self) {
        // lea rsi, [rip+disp32]  → TLS 模板地址
        let lea_pos = self.pos() + 3;
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8D, 0x35, 0, 0, 0, 0]));
        self.pending_tls_rels
            .push((lea_pos, "__dcc_tls_base".to_string()));
        // mov rdi, 0x1002 (ARCH_SET_FS)
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0xC7, 0x02, 0x00, 0x01, 0x00]));
        // mov rax, 158; syscall
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0xC0, 0x9E, 0x00, 0x00, 0x00]));
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Syscall));
    }
}
