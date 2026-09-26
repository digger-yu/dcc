typedef __builtin_va_list va_list;
long long L[8], H[8];
int f128(int a,int b,int c,int d,int e,int f,int g,int h,int i,int j,int k,...){
    long long acc=0; va_list ap; __builtin_va_start(ap,k);
    for(int n=0;n<7;n++){ __int128 v=__builtin_va_arg(ap,__int128);
        L[n]=(long long)(unsigned __int128)v; H[n]=(long long)(v>>64); acc+=L[n]+H[n]; }
    __builtin_va_end(ap); return (int)acc;
}
