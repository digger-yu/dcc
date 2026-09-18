// Test native SSA control flow lowering: for, do-while, switch, break, continue, goto, label

int test_while() {
    int i = 0;
    int sum = 0;
    while (i < 5) {
        sum += i;
        i++;
    }
    return sum;
}

int test_for() {
    int sum = 0;
    for (int i = 0; i < 5; i++) {
        sum += i;
    }
    return sum;
}

int test_do_while() {
    int i = 0;
    int sum = 0;
    do {
        sum += i;
        i++;
    } while (i < 5);
    return sum;
}

int test_break_continue() {
    int sum = 0;
    for (int i = 0; i < 10; i++) {
        if (i == 5) break;
        if (i % 2 == 0) continue;
        sum += i;
    }
    return sum;
}

int test_switch(int n) {
    int result = 0;
    switch (n) {
        case 0:
            result = 10;
            break;
        case 1:
            result = 20;
            break;
        case 2:
            result = 30;
            break;
        default:
            result = -1;
            break;
    }
    return result;
}

int test_goto_label() {
    int x = 0;
    x = 1;
    goto done;
    x = 2;
done:
    return x;
}

int main() {
    int r = test_while();
    if (r != 10) return 1;
    r = test_for();
    if (r != 10) return 2;
    r = test_do_while();
    if (r != 10) return 3;
    r = test_break_continue();
    if (r != 4) return 4;
    r = test_switch(0);
    if (r != 10) return 5;
    r = test_switch(1);
    if (r != 20) return 6;
    r = test_switch(2);
    if (r != 30) return 7;
    r = test_switch(99);
    if (r != -1) return 8;
    r = test_goto_label();
    if (r != 1) return 9;
    return 0;
}
