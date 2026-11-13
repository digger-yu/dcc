set_i:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov %rdi,-0x20(%r13)
movslq %edi,%rax
mov %eax,0x0(%rip)
xor %eax,%eax
add $0x30,%rsp
pop %r13
ret
<nop padding>
get_i:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
movslq 0x0(%rip),%rax
add $0x30,%rsp
pop %r13
ret
<nop padding>
set_l:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov %rdi,-0x20(%r13)
mov %rdi,%rax
mov %rax,0x0(%rip)
xor %eax,%eax
add $0x30,%rsp
pop %r13
ret
<nop padding>
get_l:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov 0x0(%rip),%rax
add $0x30,%rsp
pop %r13
ret
<nop padding>
set_d:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
movsd %xmm0,-0x20(%r13)
movsd -0x20(%r13),%xmm0
movsd %xmm0,0x0(%rip)
xor %eax,%eax
add $0x30,%rsp
pop %r13
ret
<nop padding>
get_d:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
movsd 0x0(%rip),%xmm0
add $0x30,%rsp
pop %r13
ret
