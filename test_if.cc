inline bool cpp_builtin_macro_p(int node) {
  return node == 1;
}

static void maybe_start_funlike(int node, int *n) {
  if (cpp_builtin_macro_p(node))
    *n = 1;
  else
    *n = 2;
}
