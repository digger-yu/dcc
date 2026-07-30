typedef void (*fn_t)(void);
extern fn_t __SCT__might_resched;
extern void *__SCK__might_resched;

void might_resched(void)
{
	({ static void *p = (void *)&__SCK__might_resched;; (&__SCT__might_resched); })();
}
