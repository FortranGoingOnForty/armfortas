#!/bin/sh
# Fail before expensive tests if a hosted-runner label stops resolving to the
# operating system or architecture that gives the job its coverage.
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <expected-uname-s> <expected-uname-m>" >&2
    exit 2
fi

expected_os=$1
expected_arch=$2
actual_os=$(uname -s)
actual_arch=$(uname -m)

if [ "$actual_os" != "$expected_os" ] || [ "$actual_arch" != "$expected_arch" ]; then
    echo "assert_host: expected host $expected_os/$expected_arch, got $actual_os/$actual_arch" >&2
    exit 1
fi

echo "assert_host: confirmed $actual_os/$actual_arch"
