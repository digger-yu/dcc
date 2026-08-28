typedef unsigned char CHAR_T;
CHAR_T *translate;
#define TRANSLATE(d) (translate ? (char) translate[(unsigned char) (d)] : (char) (d))
void f(const CHAR_T *d, const CHAR_T *dend) {
  while (d == dend) { d = dend; }
  if (TRANSLATE (*d) == '\n') return;
}
