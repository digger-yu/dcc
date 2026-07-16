#define _DECLARE_STATIC_KEY_0(name) extern struct static_key_false name
#define __PASTE(a,b) a##b
#define IS_ENABLED(x) 0
#define DECLARE_STATIC_KEY_MAYBE(cfg, name) __PASTE(_DECLARE_STATIC_KEY_, IS_ENABLED(cfg))(name)
DECLARE_STATIC_KEY_MAYBE(CONFIG_INIT_ON_ALLOC_DEFAULT_ON, init_on_alloc);

int main(void) { return 0; }
