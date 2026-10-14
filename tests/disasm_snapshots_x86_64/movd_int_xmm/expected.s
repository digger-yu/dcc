negate_f:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
movss %xmm0,-0x40(%r13)
movss -0x40(%r13),%xmm0
movabs $0x80000000,%rax
movq %rax,%xmm1
xorps %xmm1,%xmm0
<nop padding>
mov %r13,%rsp
pop %r13
ret
negate_d:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
movsd %xmm0,-0x40(%r13)
movsd -0x40(%r13),%xmm0
movabs $0x8000000000000000,%rax
movq %rax,%xmm1
xorpd %xmm1,%xmm0
<nop padding>
mov %r13,%rsp
pop %r13
ret
