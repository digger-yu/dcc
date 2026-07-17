/* Minimal config.h stub for libdecnumber_combined.c (dcc M7).
 * The real autotools-generated config.h is not produced without running
 * configure on the libdecnumber subdir; we only need a handful of
 * feature macros.  This file lives next to libdecnumber_combined.c
 * and is reachable via -I tools/libdecnumber_stub.
 */
#ifndef DCC_LIBDECNUMBER_CONFIG_H
#define DCC_LIBDECNUMBER_CONFIG_H

/* Enable stdint.h types (libdecnumber pulls in <gstdint.h> which is
 * gnulib's portability fallback when HAVE_STDINT_H is set). */
#ifndef HAVE_STDINT_H
#define HAVE_STDINT_H 1
#endif

/* Standard headers used by libdecnumber. */
#ifndef HAVE_STRING_H
#define HAVE_STRING_H 1
#endif
#ifndef HAVE_STDLIB_H
#define HAVE_STDLIB_H 1
#endif
#ifndef HAVE_STDIO_H
#define HAVE_STDIO_H 1
#endif
#ifndef HAVE_ERRNO_H
#define HAVE_ERRNO_H 1
#endif
#ifndef HAVE_MALLOC_H
#define HAVE_MALLOC_H 1
#endif
#ifndef HAVE_UNISTD_H
#define HAVE_UNISTD_H 1
#endif

/* dcc defaults to little-endian on x86_64 / aarch64 / riscv64. */
#ifndef WORDS_BIGENDIAN
/* leave undefined; libdecnumber handles WORDS_BIGENDIAN via #ifdef. */
#endif

/* Indicate we are building libdecnumber as a normal library archive
 * (not the in-libgcc special case).  This routes dconfig.h through
 * the "include config.h" branch rather than the libgcc2 path. */
#undef IN_LIBGCC2

#endif
