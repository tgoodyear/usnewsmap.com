#!/usr/bin/env bash
# Paid hours per instance from the ledger: destroy time less creation time, at the instance's hourly price.
set -euo pipefail
LEDGER=""
while [ $# -gt 0 ]; do case "$1" in --ledger) LEDGER=$2; shift 2;; *) exit 2;; esac; done
sed -E 's/"([^",]*), ([^",]*)"/\1 \2/' "$LEDGER" | awk -F, 'NR>1{t=$11-$8; if(t<0)t=0; h[$1]+=t/3600; c[$1]+=t/3600*$6}END{for(k in h)printf "%s,%.2f,%.2f\n",k,h[k],c[k]}' | sort
