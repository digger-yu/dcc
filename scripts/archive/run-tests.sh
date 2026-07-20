#!/bin/bash
set -e
export PATH=$HOME/.cargo/bin:$PATH
cd /home/ds/dcc
cargo test --bin dcc 2>&1 | grep 'test result'
