set_i:
push %r13
mov %rsp,%r13
sub $0x300,%rsp
<nop padding>
mov %rdi,-0x48(%r13)
lea 0x0(%rip),%rax
mov %rax,-0x40(%r13)
mov -0x40(%r13),%rax
push %rax
movslq -0x48(%r13),%rax
pop %rcx
mov %eax,(%rcx)
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
movslq %eax,%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
set_l:
push %r13
mov %rsp,%r13
sub $0x300,%rsp
<nop padding>
mov %rdi,-0x48(%r13)
lea 0x0(%rip),%rax
mov %rax,-0x40(%r13)
mov -0x40(%r13),%rax
push %rax
mov -0x48(%r13),%rax
pop %rcx
mov %rax,(%rcx)
xor %eax,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
get_l:
push %r13
mov %rsp,%r13
sub $0x2e0,%rsp
<nop padding>
mov 0x0(%rip),%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
set_d:
push %r13
mov %rsp,%r13
sub $0x300,%rsp
<nop padding>
movsd %xmm0,-0x48(%r13)
lea 0x0(%rip),%rax
mov %rax,-0x40(%r13)
mov -0x40(%r13),%rax
push %rax
movsd -0x48(%r13),%xmm0
pop %rcx
movsd %xmm0,(%rcx)
xor %eax,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
get_d:
push %r13
mov %rsp,%r13
sub $0x2e0,%rsp
<nop padding>
movsd 0x0(%rip),%xmm0
<nop padding>
mov %r13,%rsp
pop %r13
ret
