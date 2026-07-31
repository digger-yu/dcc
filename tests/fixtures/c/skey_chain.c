#define DECLARE_STATIC_KEY_FALSE(name) extern struct static_key_false name
#define _DECLARE_STATIC_KEY_0(name) DECLARE_STATIC_KEY_FALSE(name)
_DECLARE_STATIC_KEY_0(init_on_alloc);

int main(void) { return 0; }
