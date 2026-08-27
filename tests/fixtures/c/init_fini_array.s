# Test fixture for ELF section type verification:
# .init_array / .fini_array should be emitted as SHT_INIT_ARRAY /
# SHT_FINI_ARRAY in the final linked ELF.

.section .init_array,"aw",@init_array
.quad my_init

.section .fini_array,"aw",@fini_array
.quad my_fini

.text
.global my_init
.type my_init, @function
my_init:
    ret

.global my_fini
.type my_fini, @function
my_fini:
    ret

.global _start
_start:
    mov $60, %rax
    xor %rdi, %rdi
    syscall
