read_b:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov -0x40(%r13),%rax
mov (%rax),%eax
and $0xf,%rax
test $0x8,%rax
je <read_b>
or $0xfffffffffffffff0,%rax
movslq %eax,%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
