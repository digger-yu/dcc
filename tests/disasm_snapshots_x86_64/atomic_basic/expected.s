atomic_add:
push %r13
mov %rsp,%r13
sub $0x310,%rsp
mov %rbx,-0x10(%r13)
<nop padding>
mov %rdi,-0x50(%r13)
mov %rsi,-0x58(%r13)
mov %rdi,%rax
movslq (%rax),%rax
mov %eax,%ebx
movslq %ebx,%rax
movslq -0x58(%r13),%r8
add %r8,%rax
movslq %eax,%rax
mov -0x50(%r13),%rcx
mov %eax,(%rcx)
movslq %ebx,%rax
mov -0x10(%r13),%rbx
<nop padding>
mov %r13,%rsp
pop %r13
ret
atomic_swap:
push %r13
mov %rsp,%r13
sub $0x300,%rsp
mov %rbx,-0x10(%r13)
<nop padding>
mov %rdi,-0x48(%r13)
mov %rsi,-0x50(%r13)
mov %rdi,%rax
mov (%rax),%rax
mov %rax,%rbx
mov -0x48(%r13),%rax
mov %rax,%rcx
mov -0x50(%r13),%rax
mov %rax,(%rcx)
mov %rbx,%rax
mov -0x10(%r13),%rbx
<nop padding>
mov %r13,%rsp
pop %r13
ret
