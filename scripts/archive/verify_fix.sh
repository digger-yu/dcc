#!/bin/bash
grep -n 'contains("##")' /home/ds/dcc/src/preproc.rs || true
grep -n 'replace_all_params' /home/ds/dcc/src/preproc.rs | head -5
