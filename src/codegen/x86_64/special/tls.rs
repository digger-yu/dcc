// src/codegen/x86_64/special/tls.rs
// Thread-Local Storage (TLS) support for x86_64 backend
// FS-relative and GS-relative addressing models

/// TLS access model enum matching GCC/Clang TLS models.
#[derive(Debug, Clone, Copy)]
pub enum TlsModel {
    /// Global-dynamic: First use calls __tls_get_addr()
    GlobalDynamic,
    
    /// Local-dynamic: First use similar to global-dynamic but optimized
    LocalDynamic,
    
    /// Initial-exec: Symbol in local TLS block, no runtime lookup
    InitialExec,
    
    /// Local-exec: Absolute offset known at link time
    LocalExec,
}

/// Emit TLS address calculation based on access model.
/// 
/// Generates different code sequences depending on TLS model:
/// - LocalExec: LEA rax, [fs + absolute_offset]
/// - InitialExec: LEA rax, [fs + static_base + offset]
/// - GlobalDynamic: Call __tls_get_addr() with slot number
/// 
/// # Arguments
/// * `cg` - Codegen context
/// * `model` - TLS access model to use
/// * `base_register` - FS or GS base register (typically 4=FS, 5=GS)
/// * `slot_offset` - Offset within TLS block
pub fn emit_tls_address_calculation(cg: &mut crate::codegen::x86_64::Codegen, 
    model: TlsModel, base_register: u8, slot_offset: i64) {
    
    let rex_w = 0x48; // REX.W for 64-bit
    
    match model {
        TlsModel::LocalExec => {
            // LEA rax, [fs + disp32] - absolute offset known at link time
            cg.emit(&[rex_w, 0x8D, 0x04 | ((base_register & 7) << 3), 0x25]);
            cg.emit(&(slot_offset as i32).to_le_bytes());
        }
        TlsModel::InitialExec => {
            // Similar but may need additional PC-relative adjustment
            unimplemented!("InitialExec TLS model requires extraction from x86_64.rs");
        }
        TlsModel::GlobalDynamic => {
            // CALL __tls_get_addr(slot_number)
            // mov rdi, slot_number; call __tls_get_addr
            unimplemented!("GlobalDynamic TLS requires runtime lookup extraction");
        }
        TlsModel::LocalDynamic => {
            // Combined dynamic + initial exec sequence
            unimplemented!("LocalDynamic TLS complexity requires detailed extraction");
        }
    }
}
