j_signed:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rsi,-0x48(%r13)
movslq %edi,%rax
movslq %esi,%r8
cmp %r8d,%eax
jge <j_signed>
mov $0x1,%eax
mov %r13,%rsp
pop %r13
ret
movslq -0x40(%r13),%rax
movslq -0x48(%r13),%r8
cmp %r8d,%eax
jl <j_signed>
mov $0x2,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
mov $0x3,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
j_unsigned:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rsi,-0x48(%r13)
mov %edi,%eax
mov %esi,%r8d
cmp %r8d,%eax
jbe <j_unsigned>
mov $0x1,%eax
mov %eax,%eax
mov %r13,%rsp
pop %r13
ret
mov -0x40(%r13),%eax
mov -0x48(%r13),%r8d
cmp %r8d,%eax
ja <j_unsigned>
mov $0x2,%eax
mov %eax,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
mov $0x3,%eax
mov %eax,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
j_narrow:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
movzbl %dil,%eax
movzbq %al,%rax
cmp $0xc8,%eax
jle <j_narrow>
mov $0x1,%eax
mov %r13,%rsp
pop %r13
ret
mov $0x0,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
j_ptr:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rdi,%rax
test %rax,%rax
je <j_ptr>
mov $0x1,%eax
mov %r13,%rsp
pop %r13
ret
mov -0x40(%r13),%rax
test %rax,%rax
jne <j_ptr>
mov $0x2,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
mov $0x3,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
j_ternary:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rsi,-0x48(%r13)
movslq %edi,%rax
movslq %esi,%r8
cmp %r8d,%eax
jge <j_ternary>
mov $0xb,%eax
jmp <j_ternary>
mov $0x16,%eax
movslq %eax,%rax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
j_dowhile:
push %r13
mov %rsp,%r13
sub $0x310,%rsp
mov %rbx,-0x10(%r13)
<nop padding>
mov %rdi,-0x60(%r13)
mov $0x0,%eax
mov %eax,%ebx
mov $0x1,%eax
test %eax,%eax
je <j_dowhile>
movslq -0x60(%r13),%rax
movslq %ebx,%r8
add %r8,%rax
movslq %eax,%rax
mov %eax,%ebx
movslq -0x60(%r13),%rax
add $0x0,%rax
movslq %eax,%rax
add $0xffffffffffffffff,%rax
movslq %eax,%rax
add $0x0,%rax
movslq %eax,%rax
mov %eax,-0x60(%r13)
test %eax,%eax
jle <j_dowhile>
jmp <j_dowhile>
jmp <j_dowhile>
jmp <j_dowhile>
movslq %ebx,%rax
mov -0x10(%r13),%rbx
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
j_float_still_materializes:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
mov %rbx,-0x10(%r13)
<nop padding>
movsd %xmm0,-0x40(%r13)
movsd %xmm1,-0x48(%r13)
movsd -0x40(%r13),%xmm0
sub $0x8,%rsp
movsd %xmm0,(%rsp)
movsd -0x48(%r13),%xmm0
movapd %xmm0,%xmm1
movsd (%rsp),%xmm0
add $0x8,%rsp
ucomisd %xmm1,%xmm0
setb %al
setnp %bl
and %bl,%al
movzbq %al,%rax
test %eax,%eax
je <j_float_still_materializes>
mov $0x1,%eax
mov -0x10(%r13),%rbx
<nop padding>
mov %r13,%rsp
pop %r13
ret
mov $0x0,%eax
mov -0x10(%r13),%rbx
<nop padding>
mov %r13,%rsp
pop %r13
ret
j_bitfield_still_materializes:
push %r13
mov %rsp,%r13
sub $0x2f0,%rsp
<nop padding>
mov %rdi,-0x40(%r13)
mov %rdi,%rax
mov (%rax),%eax
and $0x7,%rax
cmp $0x3,%eax
sete %al
movzbq %al,%rax
test %eax,%eax
je <j_bitfield_still_materializes>
mov $0x1,%eax
mov %r13,%rsp
pop %r13
ret
mov $0x0,%eax
<nop padding>
mov %r13,%rsp
pop %r13
ret
<nop padding>
j_nested_value_still_materializes:
push %r13
mov %rsp,%r13
sub $0x310,%rsp
mov %rbx,-0x10(%r13)
<nop padding>
mov %rdi,-0x58(%r13)
mov %rsi,-0x60(%r13)
movslq %edi,%rax
movslq %esi,%r8
cmp %r8d,%eax
setl %al
movzbq %al,%rax
add $0x1,%rax
movslq %eax,%rax
mov %eax,%ebx
mov $0x0,%eax
mov -0x10(%r13),%rbx
mov %r13,%rsp
pop %r13
ret
<nop padding>
j_bool_result_still_materializes:
push %r13
mov %rsp,%r13
sub $0x310,%rsp
mov %rbx,-0x10(%r13)
<nop padding>
mov %rdi,-0x50(%r13)
mov %rsi,-0x58(%r13)
movslq %edi,%rax
movslq %esi,%r8
cmp %r8d,%eax
sete %al
movzbq %al,%rax
mov %eax,%ebx
movslq %ebx,%rax
test %eax,%eax
je <j_bool_result_still_materializes>
mov $0x1,%eax
mov -0x10(%r13),%rbx
mov %r13,%rsp
pop %r13
ret
mov $0x0,%eax
mov -0x10(%r13),%rbx
<nop padding>
mov %r13,%rsp
pop %r13
ret
