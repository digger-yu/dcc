/* Test: __attribute__((tls_model("..."))) TLS model attribute */

__thread int x;
__thread __attribute__((tls_model("global-dynamic"))) int y;
__thread __attribute__((tls_model("local-dynamic"))) int z;
__thread __attribute__((tls_model("initial-exec"))) int w;
__thread __attribute__((tls_model("local-exec"))) int v;

int main(void) {
    x = 1;
    y = 2;
    z = 3;
    w = 4;
    v = 5;
    return x + y + z + w + v;
}
