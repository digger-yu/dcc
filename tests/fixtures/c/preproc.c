/* M1.3: preprocessor - macro expansion, conditional compilation, include paths */
#define VERSION 2
#define STR(x) #x
#define XSTR(x) STR(x)
#define CONCAT(a, b) a##b

#if VERSION >= 2
  #define FEATURE_ENABLED 1
  #define MSG "version >= 2"
#elif VERSION == 1
  #define FEATURE_ENABLED 0
  #define MSG "version == 1"
#else
  #define FEATURE_ENABLED 0
  #define MSG "unknown version"
#endif

#ifndef GUARD_TEST
#define GUARD_TEST 42
#endif

#if defined(FEATURE_ENABLED) && FEATURE_ENABLED
int CONCAT(fe, ature) = 1;
#endif

int ver = VERSION;
int guard = GUARD_TEST;
const char *msg = MSG;
const char *verstr = XSTR(VERSION);
int CONCAT(coun, ter) = 0;

#undef FEATURE_ENABLED
#ifdef FEATURE_ENABLED
int should_not_appear = 1;
#endif

int main(void) {
    return 0;
}
