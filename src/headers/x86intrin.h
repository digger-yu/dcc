/* dcc 内置 x86intrin.h: 以标量代码模拟 libiberty/sha1.c 所需的
   SSE / SHA intrinsics. 不产生真实 SIMD 指令, 语义与 Intel SDM 一致. */
#ifndef _DCC_X86INTRIN_H
#define _DCC_X86INTRIN_H

typedef long long __m128i __attribute__((__vector_size__(16)));

typedef union {
  __m128i v;
  unsigned int u32[4];
  unsigned long long u64[2];
  unsigned char u8[16];
} __dcc_m128;

static inline unsigned int __dcc_rol32(unsigned int x, int n) {
  return (x << n) | (x >> (32 - n));
}

static inline __m128i _mm_set_epi32(int e3, int e2, int e1, int e0) {
  __dcc_m128 r;
  r.u32[0] = (unsigned int) e0;
  r.u32[1] = (unsigned int) e1;
  r.u32[2] = (unsigned int) e2;
  r.u32[3] = (unsigned int) e3;
  return r.v;
}

static inline __m128i _mm_set_epi64x(long long e1, long long e0) {
  __dcc_m128 r;
  r.u64[0] = (unsigned long long) e0;
  r.u64[1] = (unsigned long long) e1;
  return r.v;
}

static inline __m128i _mm_loadu_si128(const __m128i *p) {
  __dcc_m128 r;
  const unsigned char *s = (const unsigned char *) p;
  int i;
  for (i = 0; i < 16; i++)
    r.u8[i] = s[i];
  return r.v;
}

static inline void _mm_storeu_si128(__m128i *p, __m128i a) {
  __dcc_m128 t;
  unsigned char *d = (unsigned char *) p;
  int i;
  t.v = a;
  for (i = 0; i < 16; i++)
    d[i] = t.u8[i];
}

static inline __m128i _mm_add_epi32(__m128i a, __m128i b) {
  __dcc_m128 x, y, r;
  int i;
  x.v = a;
  y.v = b;
  for (i = 0; i < 4; i++)
    r.u32[i] = x.u32[i] + y.u32[i];
  return r.v;
}

static inline __m128i _mm_xor_si128(__m128i a, __m128i b) {
  __dcc_m128 x, y, r;
  x.v = a;
  y.v = b;
  r.u64[0] = x.u64[0] ^ y.u64[0];
  r.u64[1] = x.u64[1] ^ y.u64[1];
  return r.v;
}

static inline __m128i _mm_shuffle_epi32(__m128i a, int imm) {
  __dcc_m128 x, r;
  int i;
  x.v = a;
  for (i = 0; i < 4; i++)
    r.u32[i] = x.u32[(imm >> (2 * i)) & 3];
  return r.v;
}

static inline __m128i _mm_shuffle_epi8(__m128i a, __m128i b) {
  __dcc_m128 x, y, r;
  int i;
  x.v = a;
  y.v = b;
  for (i = 0; i < 16; i++)
    r.u8[i] = (y.u8[i] & 0x80) ? 0 : x.u8[y.u8[i] & 0x0f];
  return r.v;
}

static inline int _mm_extract_epi32(__m128i a, int imm) {
  __dcc_m128 x;
  x.v = a;
  return (int) x.u32[imm & 3];
}

/* SHA1MSG1: DEST = (W2^W0, W3^W1, W4^W2, W5^W3), 高 lane 在前 */
static inline __m128i _mm_sha1msg1_epu32(__m128i a, __m128i b) {
  __dcc_m128 x, y, r;
  x.v = a;
  y.v = b;
  r.u32[3] = x.u32[1] ^ x.u32[3];
  r.u32[2] = x.u32[0] ^ x.u32[2];
  r.u32[1] = y.u32[3] ^ x.u32[1];
  r.u32[0] = y.u32[2] ^ x.u32[0];
  return r.v;
}

/* SHA1MSG2: W16..W19 = rol1(SRC1 lane ^ W13/14/15/W16) */
static inline __m128i _mm_sha1msg2_epu32(__m128i a, __m128i b) {
  __dcc_m128 x, y, r;
  unsigned int w16, w17, w18, w19;
  x.v = a;
  y.v = b;
  w16 = __dcc_rol32(x.u32[3] ^ y.u32[2], 1);
  w17 = __dcc_rol32(x.u32[2] ^ y.u32[1], 1);
  w18 = __dcc_rol32(x.u32[1] ^ y.u32[0], 1);
  w19 = __dcc_rol32(x.u32[0] ^ w16, 1);
  r.u32[3] = w16;
  r.u32[2] = w17;
  r.u32[1] = w18;
  r.u32[0] = w19;
  return r.v;
}

/* SHA1NEXTE: DEST[127:96] = SRC2[127:96] + rol30(SRC1[127:96]) */
static inline __m128i _mm_sha1nexte_epu32(__m128i a, __m128i b) {
  __dcc_m128 x, y, r;
  x.v = a;
  y.v = b;
  r.u32[3] = y.u32[3] + __dcc_rol32(x.u32[3], 30);
  r.u32[2] = y.u32[2];
  r.u32[1] = y.u32[1];
  r.u32[0] = y.u32[0];
  return r.v;
}

/* SHA1RNDS4: 执行 4 轮 SHA1, func 选择 f/K (Intel SDM Vol.2B) */
static inline __m128i _mm_sha1rnds4_epu32(__m128i a, __m128i e_msg, int func) {
  __dcc_m128 x, y, r;
  unsigned int A, B, C, D, E, f, K, t;
  int i;
  x.v = a;
  y.v = e_msg;
  A = x.u32[3];
  B = x.u32[2];
  C = x.u32[1];
  D = x.u32[0];
  E = 0;
  if (func == 0)
    K = 0x5A827999u;
  else if (func == 1)
    K = 0x6ED9EBA1u;
  else if (func == 2)
    K = 0x8F1BBCDCu;
  else
    K = 0xCA62C1D6u;
  for (i = 0; i < 4; i++) {
    if (func == 0)
      f = (B & C) ^ (~B & D);
    else if (func == 2)
      f = (B & C) ^ (B & D) ^ (C & D);
    else
      f = B ^ C ^ D;
    t = f + __dcc_rol32(A, 5) + y.u32[3 - i] + E + K;
    E = D;
    D = C;
    C = __dcc_rol32(B, 30);
    B = A;
    A = t;
  }
  r.u32[3] = A;
  r.u32[2] = B;
  r.u32[1] = C;
  r.u32[0] = D;
  return r.v;
}

#endif /* _DCC_X86INTRIN_H */
