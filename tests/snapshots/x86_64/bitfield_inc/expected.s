inc_b:
push %r13
mov %rsp,%r13
sub $0x400,%rsp
mov %rbx,-0x10(%r13)
<nop padding>
mov %rdi,-0x50(%r13)
mov -0x50(%r13),%rax
mov %rax,-0x48(%r13)
mov -0x48(%r13),%rax
push %rax
mov -0x48(%r13),%rax
movslq (%rax),%rax
mov %rax,%rbx
mov $0x1,%r8
add %r8,%rbx
mov %rbx,%rax
movslq %eax,%rax
pop %rcx
mov %eax,(%rcx)
xor %eax,%eax
mov -0x10(%r13),%rbx
<nop padding>
mov %r13,%rsp
pop %r13
ret
