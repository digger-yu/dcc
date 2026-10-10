struct Point {
    int x;
    int y;
};

int sum_struct(struct Point p) {
    return p.x + p.y;
}

long long add64(long long a, long long b) {
    return a + b;
}

double add_double(double a, double b) {
    return a + b;
}

int array_sum(int arr[], int n) {
    int sum = 0;
    for (int i = 0; i < n; i++) {
        sum += arr[i];
    }
    return sum;
}
