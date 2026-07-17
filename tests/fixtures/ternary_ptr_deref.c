int *pick(int cond, int *a, int *b) {
	return *(cond ? a : &(*b));
}
