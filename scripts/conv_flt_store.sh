#!/bin/bash
# 一次性机械收敛（GNU sed -z 全文匹配）：把影子副本
#   self.emit_scalar_to_xmm0_if_needed(r, TY);
#   if matches!(TY, Type::Float) { movss [rcx],xmm0 } else { movsd [rcx],xmm0 }
# 替换为单一真源 helper 调用 emit_store_scalar_float_at(r, TY, base)。
set -euo pipefail
RE='([ ]*)self\.emit_scalar_to_xmm0_if_needed\(r, ([^;)]*)\);\n\1if matches!\(\2, Type::Float\) \{\n[ ]*self\.emit\(&\[0xF3, 0x0F, 0x11, 0x01\]\);[^\n]*\n\1\} else \{\n[ ]*self\.emit\(&\[0xF2, 0x0F, 0x11, 0x01\]\);[^\n]*\n\1\}'
for f in "$@"; do
  n=$(sed -z -E "s/$RE/\1#CONV#\2/g" "$f" | tr -cd '#' | wc -c)
  sed -z -i -E "s/$RE/\1self.emit_store_scalar_float_at(r, \2, 1); \/\/ 单一真源：收浮影子副本/g" "$f"
  echo "$f: converted=$n"
done
