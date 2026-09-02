/* Combined libdecnumber TU for dcc.
 *
 * libdecnumber 在 GCC 仓库里由 11 个互相 #include 的源文件组成
 * (decContext.c, decLibrary.c, decExcept.c, decRound.c, decNumber.c,
 *  decSingle.c, decDouble.c, decQuad.c, decPacked.c, decCommon.c, decBasic.c).
 * decCommon.c / decBasic.c 不是独立 TU, 是被格式 wrapper
 * (decSingle.c / decDouble.c / decQuad.c) 用不同 DECPMAX 反复
 * 包含以生成 decSingle* / decDouble* / decQuad* 三套实现.
 *
 * dcc 之前在 probe 脚本里把 11 个文件全部 SKIP, 原因是:
 *   - DECPMAX 这种每 TU 不同的宏要靠 -DDECPMAX=N 传, 在 wrapper
 *     复用 decCommon.c 时容易撞车;
 *   - decCommon.c / decBasic.c 没有 include guard, 多次直接编译会
 *     重复声明 decFloat* (重映射前) 符号.
 *
 * 此文件将 libdecnumber 收敛到一个 .c 里, 编译为单 .o 后直接参与
 * libiberty 链接. 三种格式的 wrapper 顺序 include, 每次 include
 * 之前重新定义 DECPMAX 与 decFloat* 的 #define 映射, 保持原
 * libdecnumber 的编译语义 (decCommon.c / decBasic.c 内代码因 #define
 * 重映射最终生成 decSingle* / decDouble* / decQuad* 三套符号).
 *
 * 编译:
 *   dcc -c -I $GCC_SRC/libdecnumber -I $GCC_SRC/include \
 *       -DHAVE_CONFIG_H -o libdecnumber_combined.o \
 *       tools/libdecnumber_combined.c
 *
 * 限制 (M7):
 *   - 不实现 decPacked.c (decimal128 packed 编码), decPacked 留 SKIP
 *   - 不包含 dpd/ 目录的 decimal32/decimal64/decimal128 wrapper
 *     (那个走 dpd/decimalNN.c 单独编译链路)
 */
/* 模拟 configure 生成的 config.h (M7 最小) */
/* libdecnumber 实际用到的几个关键宏:
 *   HAVE_STDINT_H     -> 提供 stdint.h 类型
 *   WORDS_BIGENDIAN   -> 大端 (此处 host 假设 LE)
 *   HAVE_STRING_H / HAVE_STDLIB_H
 * 提供空的 config.h + gstdint.h (若 stdint.h 缺失) 给 dconfig.h 用
 */
#if !defined(HAVE_STDINT_H) && !defined(IN_LIBGCC2)
#define HAVE_STDINT_H 1
#endif
#include "decContext.h"
#include "decContext.c"
#include "decLibrary.c"
#include "decExcept.c"
#include "decRound.c"
/* decNumber.c 也 include "decNumberLocal.h", 第二次 include 前 undef.
 * 避免触发 #error decNumberLocal included more than once. */
#undef DECNUMBERLOC
#include "decNumber.c"

/* --- decSingle (DECPMAX=7, decimal32) --- */
#include "decSingle.h"
#define DECPMAX     DECSINGLE_Pmax
#define DECEMIN     DECSINGLE_Emin
#define DECEMAX     DECSINGLE_Emax
#define DECEMAXD    DECSINGLE_EmaxD
#define DECBYTES    DECSINGLE_Bytes
#define DECSTRING   DECSINGLE_String
#define DECECONL    DECSINGLE_EconL
#define DECBIAS     DECSINGLE_Bias
#define DECLETS     DECSINGLE_Declets
#define DECQTINY    (-DECSINGLE_Bias)
#define DECWBYTES   DECDOUBLE_Bytes
#define DECWPMAX    DECDOUBLE_Pmax
#define DECWECONL   DECDOUBLE_EconL
#define DECWBIAS    DECDOUBLE_Bias

#define decFloat                   decSingle
#define decFloatWider              decDouble

#define decFloatFromPacked         decSingleFromPacked
#define decFloatFromPackedChecked  decSingleFromPackedChecked
#define decFloatFromString         decSingleFromString
#define decFloatFromWider          decSingleFromWider
#define decFloatGetCoefficient     decSingleGetCoefficient
#define decFloatGetExponent        decSingleGetExponent
#define decFloatSetCoefficient     decSingleSetCoefficient
#define decFloatSetExponent        decSingleSetExponent
#define decFloatShow               decSingleShow
#define decFloatToPacked           decSingleToPacked
#define decFloatToEngString        decSingleToEngString
#define decFloatToString           decSingleToString
#define decFloatToWider            decSingleToWider
#define decFloatZero               decSingleZero
#define decFloatRadix              decSingleRadix
#define decFloatVersion            decSingleVersion

/* decNumberLocal.h 用 #if !defined(DECNUMBERLOC) 做伪 guard;
 * 第二次 include 会进 #else 报 "included more than once".
 * 每次 include 前 undef. */
#undef DECNUMBERLOC
#include "decNumberLocal.h"
#include "decCommon.c"
/* decBasic.c 不包含 (decimal32 跳过 basic) */

/* --- decDouble (DECPMAX=16, decimal64) --- */
#include "decDouble.h"
#undef DECPMAX
#undef DECEMIN
#undef DECEMAX
#undef DECEMAXD
#undef DECBYTES
#undef DECSTRING
#undef DECECONL
#undef DECBIAS
#undef DECLETS
#undef DECQTINY
#undef DECWBYTES
#undef DECWPMAX
#undef DECWECONL
#undef DECWBIAS
/* decCommon.c 会基于 DECPMAX 重新定义 SINGLE/DOUBLE/QUAD, 先 undef. */
#undef SINGLE
#undef DOUBLE
#undef QUAD

#define DECPMAX     DECDOUBLE_Pmax
#define DECEMIN     DECDOUBLE_Emin
#define DECEMAX     DECDOUBLE_Emax
#define DECEMAXD    DECDOUBLE_EmaxD
#define DECBYTES    DECDOUBLE_Bytes
#define DECSTRING   DECDOUBLE_String
#define DECECONL    DECDOUBLE_EconL
#define DECBIAS     DECDOUBLE_Bias
#define DECLETS     DECDOUBLE_Declets
#define DECQTINY    (-DECDOUBLE_Bias)
#define DECWBYTES   DECQUAD_Bytes
#define DECWPMAX    DECQUAD_Pmax
#define DECWECONL   DECQUAD_EconL
#define DECWBIAS    DECQUAD_Bias

#define decFloat                   decDouble
#define decFloatWider              decQuad

#define decFloatFromBCD            decDoubleFromBCD
#define decFloatFromInt32          decDoubleFromInt32
#define decFloatFromPacked         decDoubleFromPacked
#define decFloatFromPackedChecked  decDoubleFromPackedChecked
#define decFloatFromString         decDoubleFromString
#define decFloatFromUInt32         decDoubleFromUInt32
#define decFloatFromWider          decDoubleFromWider
#define decFloatGetCoefficient     decDoubleGetCoefficient
#define decFloatGetExponent        decDoubleGetExponent
#define decFloatSetCoefficient     decDoubleSetCoefficient
#define decFloatSetExponent        decDoubleSetExponent
#define decFloatShow               decDoubleShow
#define decFloatToBCD              decDoubleToBCD
#define decFloatToEngString        decDoubleToEngString
#define decFloatToInt32            decDoubleToInt32
#define decFloatToInt32Exact       decDoubleToInt32Exact
#define decFloatToPacked           decDoubleToPacked
#define decFloatToString           decDoubleToString
#define decFloatToUInt32           decDoubleToUInt32
#define decFloatToUInt32Exact      decDoubleToUInt32Exact
#define decFloatToWider            decDoubleToWider
#define decFloatZero               decDoubleZero

#define decFloatAbs                decDoubleAbs
#define decFloatAdd                decDoubleAdd
#define decFloatAnd                decDoubleAnd
#define decFloatDivide             decDoubleDivide
#define decFloatDivideInteger      decDoubleDivideInteger
#define decFloatFMA                decDoubleFMA
#define decFloatInvert             decDoubleInvert
#define decFloatLogB               decDoubleLogB
#define decFloatMax                decDoubleMax
#define decFloatMaxMag             decDoubleMaxMag
#define decFloatMin                decDoubleMin
#define decFloatMinMag             decDoubleMinMag
#define decFloatMinus              decDoubleMinus
#define decFloatMultiply           decDoubleMultiply
#define decFloatNextMinus          decDoubleNextMinus
#define decFloatNextPlus           decDoubleNextPlus
#define decFloatNextToward         decDoubleNextToward
#define decFloatOr                 decDoubleOr
#define decFloatPlus               decDoublePlus
#define decFloatQuantize           decDoubleQuantize
#define decFloatReduce             decDoubleReduce
#define decFloatRemainder          decDoubleRemainder
#define decFloatRemainderNear      decDoubleRemainderNear
#define decFloatRotate             decDoubleRotate
#define decFloatScaleB             decDoubleScaleB
#define decFloatShift              decDoubleShift
#define decFloatSubtract           decDoubleSubtract
#define decFloatToIntegralValue    decDoubleToIntegralValue
#define decFloatToIntegralExact    decDoubleToIntegralExact
#define decFloatXor                decDoubleXor

#define decFloatCompare            decDoubleCompare
#define decFloatCompareSignal      decDoubleCompareSignal
#define decFloatCompareTotal       decDoubleCompareTotal
#define decFloatCompareTotalMag    decDoubleCompareTotalMag

#define decFloatCanonical          decDoubleCanonical
#define decFloatCopy               decDoubleCopy
#define decFloatCopyAbs            decDoubleCopyAbs
#define decFloatCopyNegate         decDoubleCopyNegate
#define decFloatCopySign           decDoubleCopySign

#define decFloatClass              decDoubleClass
#define decFloatClassString        decDoubleClassString
#define decFloatDigits             decDoubleDigits
#define decFloatIsCanonical        decDoubleIsCanonical
#define decFloatIsFinite           decDoubleIsFinite
#define decFloatIsInfinite        decDoubleIsInfinite
#define decFloatIsInteger          decDoubleIsInteger
#define decFloatIsNaN              decDoubleIsNaN
#define decFloatIsNormal           decDoubleIsNormal
#define decFloatIsSignaling        decDoubleIsSignaling
#define decFloatIsSignalling       decDoubleIsSignalling
#define decFloatIsSigned           decDoubleIsSigned
#define decFloatIsSubnormal        decDoubleIsSubnormal
#define decFloatIsZero             decDoubleIsZero
#define decFloatSameQuantum        decDoubleSameQuantum

#undef DECNUMBERLOC
#include "decCommon.c"
#include "decBasic.c"

/* --- decQuad (DECPMAX=34, decimal128) --- */
#include "decQuad.h"
#undef DECPMAX
#undef DECEMIN
#undef DECEMAX
#undef DECEMAXD
#undef DECBYTES
#undef DECSTRING
#undef DECECONL
#undef DECBIAS
#undef DECLETS
#undef DECQTINY
#undef DECWBYTES
#undef DECWPMAX
#undef DECWECONL
#undef DECWBIAS
/* decCommon.c 会基于 DECPMAX 重新定义 SINGLE/DOUBLE/QUAD, 先 undef. */
#undef SINGLE
#undef DOUBLE
#undef QUAD

#define DECPMAX     DECQUAD_Pmax
#define DECEMIN     DECQUAD_Emin
#define DECEMAX     DECQUAD_Emax
#define DECEMAXD    DECQUAD_EmaxD
#define DECBYTES    DECQUAD_Bytes
#define DECSTRING   DECQUAD_String
#define DECECONL    DECQUAD_EconL
#define DECBIAS     DECQUAD_Bias
#define DECLETS     DECQUAD_Declets
#define DECQTINY    (-DECQUAD_Bias)

#define decFloat                   decQuad
#define decFloatWider              decQuad

#define decFloatFromBCD            decQuadFromBCD
#define decFloatFromInt32          decQuadFromInt32
#define decFloatFromPacked         decQuadFromPacked
#define decFloatFromPackedChecked  decQuadFromPackedChecked
#define decFloatFromString         decQuadFromString
#define decFloatFromUInt32         decQuadFromUInt32
#define decFloatGetCoefficient     decQuadGetCoefficient
#define decFloatGetExponent        decQuadGetExponent
#define decFloatSetCoefficient     decQuadSetCoefficient
#define decFloatSetExponent        decQuadSetExponent
#define decFloatShow               decQuadShow
#define decFloatToBCD              decQuadToBCD
#define decFloatToEngString        decQuadToEngString
#define decFloatToInt32            decQuadToInt32
#define decFloatToInt32Exact       decQuadToInt32Exact
#define decFloatToPacked           decQuadToPacked
#define decFloatToString           decQuadToString
#define decFloatToUInt32           decQuadToUInt32
#define decFloatToUInt32Exact      decQuadToUInt32Exact
#define decFloatZero               decQuadZero

#define decFloatAbs                decQuadAbs
#define decFloatAdd                decQuadAdd
#define decFloatAnd                decQuadAnd
#define decFloatDivide             decQuadDivide
#define decFloatDivideInteger      decQuadDivideInteger
#define decFloatFMA                decQuadFMA
#define decFloatInvert             decQuadInvert
#define decFloatLogB               decQuadLogB
#define decFloatMax                decQuadMax
#define decFloatMaxMag             decQuadMaxMag
#define decFloatMin                decQuadMin
#define decFloatMinMag             decQuadMinMag
#define decFloatMinus              decQuadMinus
#define decFloatMultiply           decQuadMultiply
#define decFloatNextMinus          decQuadNextMinus
#define decFloatNextPlus           decQuadNextPlus
#define decFloatNextToward         decQuadNextToward
#define decFloatOr                 decQuadOr
#define decFloatPlus               decQuadPlus
#define decFloatQuantize           decQuadQuantize
#define decFloatReduce             decQuadReduce
#define decFloatRemainder          decQuadRemainder
#define decFloatRemainderNear      decQuadRemainderNear
#define decFloatRotate             decQuadRotate
#define decFloatScaleB             decQuadScaleB
#define decFloatShift              decQuadShift
#define decFloatSubtract           decQuadSubtract
#define decFloatToIntegralValue    decQuadToIntegralValue
#define decFloatToIntegralExact    decQuadToIntegralExact
#define decFloatXor                decQuadXor

#define decFloatCompare            decQuadCompare
#define decFloatCompareSignal      decQuadCompareSignal
#define decFloatCompareTotal       decQuadCompareTotal
#define decFloatCompareTotalMag    decQuadCompareTotalMag

#define decFloatCanonical          decQuadCanonical
#define decFloatCopy               decQuadCopy
#define decFloatCopyAbs            decQuadCopyAbs
#define decFloatCopyNegate         decQuadCopyNegate
#define decFloatCopySign           decQuadCopySign

#define decFloatClass              decQuadClass
#define decFloatClassString        decQuadClassString
#define decFloatDigits             decQuadDigits
#define decFloatIsCanonical        decQuadIsCanonical
#define decFloatIsFinite           decQuadIsFinite
#define decFloatIsInfinite         decQuadIsInfinite
#define decFloatIsInteger          decQuadIsInteger
#define decFloatIsNaN              decQuadIsNaN
#define decFloatIsNormal           decQuadIsNormal
#define decFloatIsSignaling        decQuadIsSignaling
#define decFloatIsSignalling       decQuadIsSignalling
#define decFloatIsSigned           decQuadIsSigned
#define decFloatIsSubnormal        decQuadIsSubnormal
#define decFloatIsZero             decQuadIsZero
#define decFloatSameQuantum        decQuadSameQuantum
#define decFloatRadix              decQuadRadix
#define decFloatVersion            decQuadVersion

#undef DECNUMBERLOC
#include "decCommon.c"
#include "decBasic.c"
