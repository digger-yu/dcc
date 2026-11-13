atomic_add:
push %r13
mov %rsp,%r13
sub $0x50,%rsp
mov %rbx,-0x10(%r13)
mov %rdi,-0x38(%r13)
mov %rsi,-0x40(%r13)
mov %rdi,%rax
movslq (%rax),%rax
mov %eax,%ebx
mov %rdi,%rax
movslq %ebx,%rax
movslq %esi,%r8
add %r8,%rax
movslq %eax,%rax
mov %rdi,%rcx
mov %eax,(%rcx)
movslq %ebx,%rax
mov -0x10(%r13),%rbx
add $0x50,%rsp
pop %r13
ret
<nop padding>
atomic_swap:
push %r13
mov %rsp,%r13
sub $0x50,%rsp
mov %rbx,-0x10(%r13)
mov %rdi,-0x30(%r13)
mov %rsi,-0x38(%r13)
mov %rdi,%rax
mov (%rax),%rax
mov %rax,%rbx
mov %rdi,%rax
mov %rax,%rcx
mov %rsi,%rax
mov %rax,(%rcx)
mov %rbx,%rax
mov -0x10(%r13),%rbx
add $0x50,%rsp
pop %r13
ret
