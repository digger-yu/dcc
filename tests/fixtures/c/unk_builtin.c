void __builtin_dcc_unknown_void(void);

int use_unknown_int(void) {
	return __builtin_dcc_unknown_test(1, 2);
}

int use_unknown_int_stmt(void) {
	__builtin_dcc_unknown_stmt(3, 4);
	return 0;
}

void use_unknown_void(void) {
	__builtin_dcc_unknown_void();
}

int main(void) {
	return use_unknown_int() + use_unknown_int_stmt();
}
