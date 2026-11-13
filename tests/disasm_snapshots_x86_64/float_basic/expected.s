muladd_f:
push %r13
mov %rsp,%r13
sub $0x50,%rsp
movss %xmm0,-0x28(%r13)
movss %xmm1,-0x30(%r13)
movss %xmm2,-0x38(%r13)
movss -0x28(%r13),%xmm0
sub $0x8,%rsp
movss %xmm0,(%rsp)
movss -0x30(%r13),%xmm0
movapd %xmm0,%xmm1
movss (%rsp),%xmm0
add $0x8,%rsp
mulss %xmm1,%xmm0
sub $0x8,%rsp
movss %xmm0,(%rsp)
movss -0x38(%r13),%xmm0
movapd %xmm0,%xmm1
movss (%rsp),%xmm0
add $0x8,%rsp
addss %xmm1,%xmm0
add $0x50,%rsp
pop %r13
ret
<nop padding>
muladd_d:
push %r13
mov %rsp,%r13
sub $0x50,%rsp
movsd %xmm0,-0x28(%r13)
movsd %xmm1,-0x30(%r13)
movsd %xmm2,-0x38(%r13)
movsd -0x28(%r13),%xmm0
sub $0x8,%rsp
movsd %xmm0,(%rsp)
movsd -0x30(%r13),%xmm0
movapd %xmm0,%xmm1
movsd (%rsp),%xmm0
add $0x8,%rsp
mulsd %xmm1,%xmm0
sub $0x8,%rsp
movsd %xmm0,(%rsp)
movsd -0x38(%r13),%xmm0
movapd %xmm0,%xmm1
movsd (%rsp),%xmm0
add $0x8,%rsp
addsd %xmm1,%xmm0
add $0x50,%rsp
pop %r13
ret
<nop padding>
recip:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
movsd %xmm0,-0x20(%r13)
movsd 0x0(%rip),%xmm0
sub $0x8,%rsp
movsd %xmm0,(%rsp)
movsd -0x20(%r13),%xmm0
movapd %xmm0,%xmm1
movsd (%rsp),%xmm0
add $0x8,%rsp
divsd %xmm1,%xmm0
add $0x30,%rsp
pop %r13
ret
