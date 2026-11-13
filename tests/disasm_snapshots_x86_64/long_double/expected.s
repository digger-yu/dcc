ld_identity:
push %r13
mov %rsp,%r13
sub $0x40,%rsp
fldt 0x10(%r13)
fstpt -0x28(%r13)
fldt -0x28(%r13)
add $0x40,%rsp
pop %r13
ret
<nop padding>
ld_add:
push %r13
mov %rsp,%r13
sub $0x50,%rsp
fldt 0x10(%r13)
fstpt -0x28(%r13)
fldt 0x20(%r13)
fstpt -0x38(%r13)
fldt -0x28(%r13)
sub $0x10,%rsp
fstpt (%rsp)
fldt -0x38(%r13)
fldt (%rsp)
add $0x10,%rsp
faddp %st,%st(1)
add $0x50,%rsp
pop %r13
ret
<nop padding>
ld_mul:
push %r13
mov %rsp,%r13
sub $0x50,%rsp
fldt 0x10(%r13)
fstpt -0x28(%r13)
fldt 0x20(%r13)
fstpt -0x38(%r13)
fldt -0x28(%r13)
sub $0x10,%rsp
fstpt (%rsp)
fldt -0x38(%r13)
fldt (%rsp)
add $0x10,%rsp
fmulp %st,%st(1)
add $0x50,%rsp
pop %r13
ret
