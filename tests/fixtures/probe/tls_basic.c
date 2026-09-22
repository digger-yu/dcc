/* TLS 探针: 一个已初始化 (应落 .tdata) + 一个未初始化 (应落 .tbss) */
_Thread_local int tls_init = 7;
_Thread_local int tls_zero;
int tls_use(int x) { tls_init += x; return tls_init + tls_zero; }
