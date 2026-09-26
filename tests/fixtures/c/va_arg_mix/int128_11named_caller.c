extern long long L[8], H[8];
extern int printf(const char*,...);
static __int128 mk(long long hi,long long lo){unsigned __int128 u=(unsigned __int128)(unsigned long long)lo;u|=((unsigned __int128)(unsigned long long)hi)<<64;return (__int128)u;}
int f128(int,int,int,int,int,int,int,int,int,int,int,...);
int main(void){
    int r=f128(1,2,3,4,5,6,7,8,9,10,11, mk(1,10),mk(2,20),mk(3,30),mk(4,40),mk(5,50),mk(6,60),mk(7,70));
    for(int n=0;n<7;n++) printf("n=%d lo=%lld hi=%lld  (期望 lo=%d hi=%d)\n", n, L[n], H[n], (n+1)*10, n+1);
    printf("r=%d (期望 308)\n", r); return 0;
}
