#!/usr/bin/env bash
# Paid hours by study from the ledger, GPU-busy share from the wrapper's samples. Paid time runs from creation to destroy.
set -euo pipefail
W=/tank/ocr/gpu-studies-202611; cd "$W"
awk -F, 'NR>1{n=NF; s=$1; gsub(/[0-9]+$/,"",s); t=$(n-3)-$(n-6); if(t<0)t=0; print $1","$2","t","$6}' ledger.csv
