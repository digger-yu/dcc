struct page { unsigned long flags; int x; };
struct folio { unsigned long flags; int y; };

_Static_assert(__builtin_offsetof(struct page, flags) == __builtin_offsetof(struct folio, flags), "flags offset");

int main(void) { return 0; }
