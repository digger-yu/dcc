/* Test: #line directive, #pragma ident, #pragma sccs */

/* #line directive */
#line 100 "remapped.c"
int at_line_100;  /* should appear at logical line 100 */

#line 200
int at_line_200;  /* should appear at logical line 200 */

/* Reset to reasonable values */
#line 1
int at_line_1;

/* #pragma ident - SCCS identification (silently accepted) */
#pragma ident "@(#)test_pragma.c 1.0"

/* #pragma sccs - SCCS identification (silently accepted) */
#pragma sccs "@(#)test_pragma.c 1.0"

int after_pragmas;
