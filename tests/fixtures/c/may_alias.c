struct __attribute__((__may_alias__)) aliased {
	int x;
};

int use(struct aliased *p) { return p->x; }

int main(void) {
	struct aliased a = { 42 };
	return use(&a);
}
