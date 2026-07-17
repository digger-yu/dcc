typedef float v4f __attribute__((vector_size(16)));
int main(void) { v4f a, b; return (int)(a | b)[0]; }
