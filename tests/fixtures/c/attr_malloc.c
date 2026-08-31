/* Test: __attribute__((malloc, alloc_size, assume_aligned,
 *         warn_unused_result, sentinel, format_arg)) */

/* malloc: returned pointer does not alias existing memory */
__attribute__((malloc))
void *my_alloc(unsigned long size);

/* alloc_size: argument 1 specifies allocation size */
__attribute__((malloc, alloc_size(1)))
void *my_malloc(unsigned long size);

/* alloc_size with two arguments: rows * cols */
__attribute__((malloc, alloc_size(1, 2)))
void *my_calloc(unsigned long n, unsigned long m);

/* assume_aligned: returned pointer aligned to 64 bytes */
__attribute__((malloc, assume_aligned(64)))
void *aligned_alloc(unsigned long size);

/* warn_unused_result */
__attribute__((warn_unused_result))
int check_permission(int uid);

/* sentinel: variadic function must be NULL-terminated */
__attribute__((sentinel))
int execlp(const char *file, ...);

/* format_arg: argument 1 is a format string */
__attribute__((format_arg(1)))
const char *my_gettext(const char *msgid);

/* Combined attributes */
__attribute__((malloc, alloc_size(1), assume_aligned(16), warn_unused_result))
void *combined_alloc(unsigned long size);

int main(void) {
    void *p = my_malloc(128);
    (void)p;
    return 0;
}
