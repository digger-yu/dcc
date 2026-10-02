void __builtin_dcc_unknown_void(void);
int __builtin_dcc_unknown_test(int, int);
int __builtin_dcc_unknown_stmt(int, int);
double __builtin_dcc_unknown_double(void);

typedef int v4si __attribute__((vector_size(16)));
struct big_unknown {
	long a;
	long b;
	long c;
};
v4si __builtin_dcc_unknown_vector(void);
struct big_unknown __builtin_dcc_unknown_sret(void);

int use_unknown_int(void) {
	return __builtin_dcc_unknown_test(1, 2);
}

int use_unknown_int_stmt(void) {
	__builtin_dcc_unknown_stmt(3, 4);
	return 0;
}

double use_unknown_double(void) {
	return __builtin_dcc_unknown_double();
}

v4si use_unknown_vector(void) {
	return __builtin_dcc_unknown_vector();
}

struct big_unknown use_unknown_sret(void) {
	return __builtin_dcc_unknown_sret();
}

void use_unknown_void(void) {
	__builtin_dcc_unknown_void();
}

int main(void) {
	v4si v = use_unknown_vector();
	struct big_unknown s = use_unknown_sret();
	return use_unknown_int() + use_unknown_int_stmt()
		+ (use_unknown_double() != 0.0)
		+ v[0] + v[1] + v[2] + v[3]
		+ (int)(s.a + s.b + s.c);
}
