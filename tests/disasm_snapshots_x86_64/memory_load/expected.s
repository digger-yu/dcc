l_b:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov %rdi,-0x20(%r13)
mov %rdi,%rax
movsbq (%rax),%rax
movsbq %al,%rax
add $0x30,%rsp
pop %r13
ret
<nop padding>
l_s:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov %rdi,-0x20(%r13)
mov %rdi,%rax
movswq (%rax),%rax
movswq %ax,%rax
add $0x30,%rsp
pop %r13
ret
<nop padding>
l_i:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov %rdi,-0x20(%r13)
mov %rdi,%rax
movslq (%rax),%rax
add $0x30,%rsp
pop %r13
ret
l_l:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov %rdi,-0x20(%r13)
mov %rdi,%rax
mov (%rax),%rax
add $0x30,%rsp
pop %r13
ret
l_off:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov %rdi,-0x20(%r13)
mov %rdi,%rax
add $0x18,%rax
mov (%rax),%rax
add $0x30,%rsp
pop %r13
ret
<nop padding>
l_glob:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov 0x0(%rip),%rax
add $0x30,%rsp
pop %r13
ret
