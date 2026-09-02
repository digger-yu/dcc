/* examples/dev/dfsan_track_taint.c
 *
 * DFSan (-fsanitize=dataflow) example:
 *   - Tracks data flow through memory operations via taint labels.
 *   - Each load retrieves the label associated with the memory
 *     location via __dfsan_load_label_N.
 *   - Each store propagates the current expression label to the
 *     target via __dfsan_store_label_N.
 *   - __dfsan_func_entry is called at function prologue to
 *     initialize the per-function label stack.
 *
 * Expected sanitizer callbacks:
 *   __dfsan_func_entry      (every function entry)
 *   __dfsan_load_label_4    (int load — retrieves taint label)
 *   __dfsan_store_label_4   (int store — propagates taint label)
 */

int propagate_taint(int *src, int *dst) {
    int val = *src;   /* load: __dfsan_load_label_4(&*src) */
    *dst = val;       /* store: __dfsan_store_label_4(&*dst, label) */
    return val;
}

int taint_arithmetic(int *a, int *b) {
    int x = *a;       /* load with label */
    int y = *b;       /* load with label */
    return x + y;     /* result carries union of both labels */
}
