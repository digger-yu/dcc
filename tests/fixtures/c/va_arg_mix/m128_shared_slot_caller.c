typedef double v2df __attribute__((vector_size(16)));
int probe(int, ...);
int main(void) { v2df x, y; x[0]=1.5; x[1]=2.5; y[0]=3.5; y[1]=4.5; return probe(0, x, y); }
