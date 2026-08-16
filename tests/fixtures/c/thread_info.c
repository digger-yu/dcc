#define current ((void *)0)
#define current_thread_info() ((struct thread_info *)current)
struct thread_info {
	unsigned long flags;
	unsigned long syscall_work;
};
int const_test_bit(int nr, unsigned long *addr);
#define bitop(op, nr, addr) \
	((__builtin_constant_p(nr) ? const##op(nr, addr) : op(nr, addr)))
#define _test_bit(nr, addr) (addr)
#define test_bit(nr, addr) bitop(_test_bit, nr, addr)
#define test_syscall_work(fl) test_bit(1, &current_thread_info()->syscall_work)
int f(void) {
	return test_syscall_work(X);
}
