ld_identity:
push %r13
mov %rsp,%r13
sub $0xc0,%rsp
<nop padding>
fldt 0x10(%r13)
fstpt -0x48(%r13)
fldt -0x48(%r13)
<nop padding>
add $0xc0,%rsp
pop %r13
ret
<nop padding>
ld_add:
push %r13
mov %rsp,%r13
sub $0xd0,%rsp
<nop padding>
fldt 0x10(%r13)
fstpt -0x48(%r13)
fldt 0x20(%r13)
fstpt -0x58(%r13)
fldt -0x48(%r13)
sub $0x10,%rsp
fstpt (%rsp)
fldt -0x58(%r13)
fldt (%rsp)
add $0x10,%rsp
faddp %st,%st(1)
<nop padding>
add $0xd0,%rsp
pop %r13
ret
<nop padding>
ld_mul:
push %r13
mov %rsp,%r13
sub $0xd0,%rsp
<nop padding>
fldt 0x10(%r13)
fstpt -0x48(%r13)
fldt 0x20(%r13)
fstpt -0x58(%r13)
fldt -0x48(%r13)
sub $0x10,%rsp
fstpt (%rsp)
fldt -0x58(%r13)
fldt (%rsp)
add $0x10,%rsp
fmulp %st,%st(1)
<nop padding>
add $0xd0,%rsp
pop %r13
ret
