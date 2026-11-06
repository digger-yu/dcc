l_b:
push %r13
mov %rsp,%r13
sub $0xc0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rdi,%rax
movsbq (%rax),%rax
movsbq %al,%rax
add $0xc0,%rsp
pop %r13
ret
<nop padding>
l_s:
push %r13
mov %rsp,%r13
sub $0xc0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rdi,%rax
movswq (%rax),%rax
movswq %ax,%rax
add $0xc0,%rsp
pop %r13
ret
<nop padding>
l_i:
push %r13
mov %rsp,%r13
sub $0xc0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rdi,%rax
movslq (%rax),%rax
add $0xc0,%rsp
pop %r13
ret
l_l:
push %r13
mov %rsp,%r13
sub $0xc0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rdi,%rax
mov (%rax),%rax
add $0xc0,%rsp
pop %r13
ret
l_off:
push %r13
mov %rsp,%r13
sub $0xc0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rdi,%rax
add $0x18,%rax
mov (%rax),%rax
add $0xc0,%rsp
pop %r13
ret
<nop padding>
l_glob:
push %r13
mov %rsp,%r13
sub $0xb0,%rsp
<nop padding>
mov 0x0(%rip),%rax
add $0xb0,%rsp
pop %r13
ret
