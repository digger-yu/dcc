read_cr4:
push %r13
mov %rsp,%r13
sub $0x50,%rsp
<nop padding>
lea -0x40(%r13),%rax
mfence
rex mov %cr4,%rax
mov %rax,-0x40(%r13)
mfence
<nop padding>
add $0x50,%rsp
pop %r13
ret
write_cr3:
push %r13
mov %rsp,%r13
sub $0x50,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rdi,%rax
mfence
rex mov %rax,%cr3
mfence
xor %eax,%eax
<nop padding>
add $0x50,%rsp
pop %r13
ret
<nop padding>
read_dr7:
push %r13
mov %rsp,%r13
sub $0x50,%rsp
<nop padding>
lea -0x40(%r13),%rax
mfence
rex mov %db7,%rax
mov %rax,-0x40(%r13)
mfence
<nop padding>
add $0x50,%rsp
pop %r13
ret
