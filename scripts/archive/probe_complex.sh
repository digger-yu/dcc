#!/usr/bin/env bash
set -euo pipefail
DCC=./target/release/dcc
run() {
  local name="$1"
  shift
  echo "=== $name"
  printf '%s\n' "$@" > /tmp/t.c
  $DCC -std=gnu23 -o /tmp/t /tmp/t.c
  /tmp/t && echo exit=0 || echo exit=$?
}
run empty 'int main(void){return 0;}'
run add_assign 'int main(void){_Complex float a=3,b=1; _Complex float c=a+b; return 0;}'
run add_cast 'int main(void){_Complex float a=3,b=1; return (int)(a+b);}'
run div_cast 'int main(void){_Complex float a=3,b=1; return (int)(a/b);}'
