// __attribute__((visibility(...))) — ELF symbol visibility.
// Tests: default/hidden/protected visibility for functions and variables.
// Compile with -c and verify ELF symbol table entries.

// default visibility (normal export)
int default_func(void) { return 1; }

// hidden visibility — not exported to dynamic symbol table
__attribute__((visibility("hidden")))
int hidden_func(void) { return 2; }

// protected visibility — exported but not preemptible
__attribute__((visibility("protected")))
int protected_func(void) { return 3; }

// global variables with visibility
int default_var = 10;

__attribute__((visibility("hidden")))
int hidden_var = 20;

int main(void) {
    return default_func() + hidden_func() + protected_func()
         + default_var + hidden_var;
}
