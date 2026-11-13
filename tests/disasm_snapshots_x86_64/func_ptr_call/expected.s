call_with:
push %r13
mov %rsp,%r13
sub $0x40,%rsp
mov %rdi,-0x20(%r13)
mov %rsi,-0x28(%r13)
sub $0x20,%rsp
mov %rdi,%rax
mov %rax,0x18(%rsp)
movslq -0x28(%r13),%rax
mov %rax,0x10(%rsp)
mov 0x10(%rsp),%rdi
mov $0x0,%al
mov 0x18(%rsp),%r11
call *%r11
add $0x20,%rsp
movslq %eax,%rax
add $0x40,%rsp
pop %r13
ret
<nop padding>
caller_calls:
push %r13
mov %rsp,%r13
sub $0x40,%rsp
mov %rdi,-0x28(%r13)
mov 0x0(%rip),%rax
mov %rax,-0x20(%r13)
sub $0x20,%rsp
mov %rax,0x18(%rsp)
movslq -0x28(%r13),%rax
mov %rax,0x10(%rsp)
mov 0x10(%rsp),%rdi
mov $0x0,%al
mov 0x18(%rsp),%r11
call *%r11
add $0x20,%rsp
movslq %eax,%rax
add $0x40,%rsp
pop %r13
ret
