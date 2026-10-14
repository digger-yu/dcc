l_b:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov -0x40(%r13),%rax
movsbq (%rax),%rax
movsbq %al,%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
l_s:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov -0x40(%r13),%rax
movswq (%rax),%rax
movswq %ax,%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
l_i:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov -0x40(%r13),%rax
movslq (%rax),%rax
movslq %eax,%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
l_l:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov -0x40(%r13),%rax
mov (%rax),%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
l_off:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov -0x40(%r13),%rax
add $0x18,%rax
mov (%rax),%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
l_glob:
push %r13
mov %rsp,%r13
sub $0x2e0,%rsp
<nop padding>
mov 0x0(%rip),%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
