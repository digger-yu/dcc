/* switch/case range syntax (GNU extension) + break/continue test */
int main(void) {
    int x = 5;
    int result = 0;

    /* basic switch */
    switch (x) {
    case 1:
        result = 10;
        break;
    case 5:
        result = 50;
        break;
    case 10:
        result = 100;
        break;
    default:
        result = -1;
        break;
    }
    if (result != 50) return 1;

    /* case range (GNU extension): case low ... high */
    x = 7;
    result = 0;
    switch (x) {
    case 1 ... 3:
        result = 100;
        break;
    case 5 ... 9:
        result = 200;
        break;
    case 10 ... 20:
        result = 300;
        break;
    default:
        result = -1;
        break;
    }
    if (result != 200) return 2;

    /* switch in loop with break/continue */
    int sum = 0;
    for (int i = 0; i < 5; i++) {
        switch (i) {
        case 0:
            continue;
        case 2:
            break;
        case 4:
            continue;
        default:
            sum += i;
            break;
        }
    }
    /* i=0: continue, i=1: sum+=1, i=2: break, i=3: sum+=3, i=4: continue */
    if (sum != 4) return 3;

    return 0;
}
