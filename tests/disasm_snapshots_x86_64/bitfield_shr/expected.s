read_b:
push %r13
mov %rsp,%r13
sub $0x30,%rsp
mov %rdi,-0x20(%r13)
mov %rdi,%rax
mov (%rax),%eax
and $0xf,%rax
test $0x8,%rax
je <read_b>
or $0xfffffffffffffff0,%rax
movslq %eax,%rax
add $0x30,%rsp
pop %r13
ret
