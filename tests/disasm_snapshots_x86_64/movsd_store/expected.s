store_d:
push %r13
mov %rsp,%r13
sub $0x40,%rsp
mov %rdi,-0x28(%r13)
movsd %xmm0,-0x20(%r13)
lea 0x0(%rip),%rax
push %rax
movslq %edi,%rcx
imul $0x8,%rcx,%rcx
pop %rax
add %rcx,%rax
push %rax
movsd -0x20(%r13),%xmm0
pop %rcx
movsd %xmm0,(%rcx)
xor %eax,%eax
add $0x40,%rsp
pop %r13
ret
<nop padding>
load_d:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov %rdi,-0x20(%r13)
movslq %edi,%rcx
lea 0x0(%rip),%r11
movsd (%r11,%rcx,8),%xmm0
add $0x30,%rsp
pop %r13
ret
