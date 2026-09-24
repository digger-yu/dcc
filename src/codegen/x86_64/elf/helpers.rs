// src/codegen/x86_64/elf/helpers.rs
// Small ELF-specific helpers: visibility parsing and label-diff patching.

use crate::codegen::shared::elf::{STV_DEFAULT, STV_HIDDEN, STV_PROTECTED};
use crate::codegen::x86_64::Codegen;

impl Codegen {
    pub fn parse_visibility(vis: &Option<String>) -> u8 {
        match vis.as_deref() {
            Some("hidden") => STV_HIDDEN,
            Some("protected") => STV_PROTECTED,
            _ => STV_DEFAULT,
        }
    }

    pub fn patch_label_diff_rels(&mut self) {
        for (data_off, label_a, label_b, width) in self.pending_label_diff_rels.drain(..) {
            if let (Some(&off_a), Some(&off_b)) =
                (self.labels.get(&label_a), self.labels.get(&label_b))
            {
                let diff = (off_a as i64) - (off_b as i64);
                let p = data_off as usize;
                if p + width <= self.data_section.len() {
                    let bytes = match width {
                        1 => (diff as i8).to_le_bytes().to_vec(),
                        2 => (diff as i16).to_le_bytes().to_vec(),
                        4 => (diff as i32).to_le_bytes().to_vec(),
                        _ => diff.to_le_bytes()[..width.min(8)].to_vec(),
                    };
                    self.data_section[p..p + bytes.len()].copy_from_slice(&bytes);
                }
            }
        }
    }
}
