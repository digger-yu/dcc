// Test: __asm__("label") GNU asm label persistence
// The asm label should override the ELF symbol name

// Variable with asm label
int my_var __asm__("custom_var_name");

// Function with asm label
void my_func(void) __asm__("custom_func_name");

// Variable with asm label and initialization
int initialized_var __asm__("my_init_var") = 42;

// Function definition with asm label
void defined_func(void) __asm__("my_defined_func") {
    // Function body
}

// Use the variables and functions
int main(void) {
    my_var = 10;
    my_func();
    initialized_var += 1;
    defined_func();
    return 0;
}
