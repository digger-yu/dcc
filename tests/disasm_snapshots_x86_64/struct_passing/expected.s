sum_pair:
push %r13
mov %rsp,%r13
sub $0x40,%rsp
mov %rdi,-0x28(%r13)
mov %rsi,-0x20(%r13)
lea -0x28(%r13),%rax
mov (%rax),%rax
mov %rax,-0x10(%r13)
lea -0x28(%r13),%r8
add $0x8,%r8
mov (%r8),%r8
add %r8,%rax
add $0x40,%rsp
pop %r13
ret
<nop padding>
sum_ptf:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
movsd %xmm0,-0x20(%r13)
lea -0x20(%r13),%rax
movss (%rax),%xmm0
sub $0x8,%rsp
movss %xmm0,(%rsp)
lea -0x20(%r13),%rax
add $0x4,%rax
movss (%rax),%xmm0
movapd %xmm0,%xmm1
movss (%rsp),%xmm0
add $0x8,%rsp
addss %xmm1,%xmm0
add $0x30,%rsp
pop %r13
ret
<nop padding>
make_pair:
push %r13
mov %rsp,%r13
sub $0x60,%rsp
mov %rdi,-0x40(%r13)
mov %rsi,-0x48(%r13)
lea -0x28(%r13),%rax
mov %rax,-0x30(%r13)
mov %rax,%rcx
mov %rdi,%rax
mov %rax,(%rcx)
lea -0x28(%r13),%rax
add $0x8,%rax
mov %rax,-0x38(%r13)
mov %rax,%rcx
mov -0x48(%r13),%rax
mov %rax,(%rcx)
mov -0x28(%r13),%rax
mov -0x20(%r13),%rdx
add $0x60,%rsp
pop %r13
ret
