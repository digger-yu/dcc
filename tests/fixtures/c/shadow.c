char *index(const char *s, int c) { (void)s; (void)c; return (char *)0; }

int shadow_in_function(void) {
	unsigned int index = 7;
	char *basename = (char *)"modpost.o";
	return (int)(index + (basename[0] == 'm'));
}

int main(void) { return shadow_in_function() == 8 ? 0 : 1; }
