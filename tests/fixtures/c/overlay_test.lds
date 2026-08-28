// Test: Linker script OVERLAY support
// OVERLAY allows multiple sections to share the same VMA
// but have different LMA (load addresses).

// This is a linker script test file.
// It would be used with: dcc -T overlay_test.lds input.o -o output

OUTPUT_FORMAT("elf64-x86-64")
ENTRY(_start)

SECTIONS {
    . = 0x1000;

    .text : {
        *(.text)
    }

    // OVERLAY block: .data1 and .data2 share the same VMA
    // but can be loaded at different addresses
    OVERLAY 0x2000 : AT(0x10000) {
        .data1 {
            *(.data1)
        }
        .data2 {
            *(.data2)
        }
    }

    // OVERLAY with NOCROSSREFS (prevents cross-references between overlaid sections)
    OVERLAY : NOCROSSREFS {
        .overlay_a {
            _start_a = .;
            *(.overlay_a)
            _end_a = .;
        }
        .overlay_b {
            _start_b = .;
            *(.overlay_b)
            _end_b = .;
        }
    }

    _end = .;
}
