void missing_named_label(void) {
	asm goto("jmp %l[missing_label]" : : : : real_label);
real_label:
	;
}

void out_of_range_label_index(void) {
	asm goto("jmp %l2" : : : : only_label);
only_label:
	;
}
