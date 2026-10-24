set_i:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
movslq %edi,%rax
mov %eax,0x0(%rip)
xor %eax,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
get_i:
push %r13
mov %rsp,%r13
sub $0x2e0,%rsp
<nop padding>
movslq 0x0(%rip),%rax
mov %r13,%rsp
pop %r13
ret
<nop padding>
set_l:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rdi,%rax
mov %rax,0x0(%rip)
xor %eax,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
get_l:
push %r13
mov %rsp,%r13
sub $0x2e0,%rsp
<nop padding>
mov 0x0(%rip),%rax
mov %r13,%rsp
pop %r13
ret
<nop padding>
set_d:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
movsd %xmm0,-0x40(%r13)
movsd -0x40(%r13),%xmm0
movsd %xmm0,0x0(%rip)
xor %eax,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
get_d:
push %r13
mov %rsp,%r13
sub $0x2e0,%rsp
<nop padding>
movsd 0x0(%rip),%xmm0
mov %r13,%rsp
pop %r13
ret
