store_d:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x48(%r13)
movsd %xmm0,-0x40(%r13)
lea 0x0(%rip),%rax
push %rax
movslq -0x48(%r13),%rcx
imul $0x8,%rcx,%rcx
pop %rax
add %rcx,%rax
push %rax
movsd -0x40(%r13),%xmm0
pop %rcx
movsd %xmm0,(%rcx)
xor %eax,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
load_d:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
movslq %edi,%rcx
lea 0x0(%rip),%r11
movsd (%r11,%rcx,8),%xmm0
mov %r13,%rsp
pop %r13
ret
