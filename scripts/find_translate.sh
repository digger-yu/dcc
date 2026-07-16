#!/bin/bash
grep -rn 'define TRANSLATE' /home/ds/gcc-src/ 2>/dev/null | head -20
grep -rn 'define PREFETCH' /home/ds/gcc-src/ 2>/dev/null | head -10
