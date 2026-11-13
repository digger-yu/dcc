negate_f:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
movss %xmm0,-0x20(%r13)
movss -0x20(%r13),%xmm0
movabs $0x80000000,%rax
movq %rax,%xmm1
xorps %xmm1,%xmm0
add $0x30,%rsp
pop %r13
ret
<nop padding>
negate_d:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
movsd %xmm0,-0x20(%r13)
movsd -0x20(%r13),%xmm0
movabs $0x8000000000000000,%rax
movq %rax,%xmm1
xorpd %xmm1,%xmm0
add $0x30,%rsp
pop %r13
ret
