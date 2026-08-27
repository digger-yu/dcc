// Test fixture for symbol versioning end-to-end verification
__asm__(".symver foo_v1,foo@v1");
__asm__(".symver foo_v2,foo@@v2");

int foo_v1(void) { return 1; }
int foo_v2(void) { return 2; }

int call_foo(void) { return foo_v2(); }
