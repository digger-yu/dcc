/* Test: C89 #line directive for debug info line number mapping */

/* Before #line: normal line numbers */
int a = 1;

/* Remap line number to 100 */
#line 100 "remapped_file.c"
int b = 2;   /* This should be at line 100 */
int c = 3;   /* This should be at line 101 */

/* Remap with just line number, no filename */
#line 200
int d = 4;   /* This should be at line 200 */

/* Verify __LINE__ reflects the remapping */
int e = __LINE__;  /* Should be 201 */

/* Remap back to normal */
#line 50
int f = 5;   /* This should be at line 50 */

/* Test __FILE__ is also affected */
const char *file = __FILE__;  /* Should be "remapped_file.c" from earlier */

int main(void) {
    return 0;
}
