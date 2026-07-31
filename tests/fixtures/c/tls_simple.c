_Thread_local int tls_counter = 10;

int main(void) {
    tls_counter += 5;
    return tls_counter;
}
