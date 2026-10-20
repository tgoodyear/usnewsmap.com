# Scoped release of #932's batched steps onto an exact live image: the bridge job's
# (bridge@91539ee1…, built at 1ac92dcb) or the hand-off job's (bridge@b4f2f00b…, built at
# f7ba230d). Both bases hold main's pre-#932 bridge-job.sh and handoff-job.sh unchanged, so
# copying the two scripts adds only JOB_IDS / HANDOFF_SPOOLS; the binary and everything else
# stay as they are. It also carries batch-inventory.py's scandir walk (#937), likewise
# unchanged on main between those builds and #937. Build it from a context holding just the
# three files at their paths:
#
#   az acr build --registry <registry> --image usnm-boinc-bridge:<commit>-batch-<job> \
#     --build-arg RUNTIME_IMAGE=<live image> \
#     --file contrib-compute/images/server/batch-overlay.Containerfile <context>
ARG RUNTIME_IMAGE
FROM ${RUNTIME_IMAGE}
COPY contrib-compute/images/server/bridge-job.sh /usr/local/bin/bridge-job
COPY contrib-compute/images/bridge/handoff-job.sh /usr/local/lib/usnm-boinc/handoff-job.sh
COPY contrib-compute/images/server/batch-inventory.py /usr/local/lib/usnm-boinc/batch-inventory.py
RUN chmod 0755 /usr/local/bin/bridge-job /usr/local/lib/usnm-boinc/handoff-job.sh \
 && bash -n /usr/local/bin/bridge-job && bash -n /usr/local/lib/usnm-boinc/handoff-job.sh \
 && grep -q 'HANDOFF_SPOOLS' /usr/local/bin/bridge-job \
 && grep -q 'JOB_IDS' /usr/local/lib/usnm-boinc/handoff-job.sh \
 && grep -q 'def spool_candidates' /usr/local/lib/usnm-boinc/batch-inventory.py \
 && python3 -m py_compile /usr/local/lib/usnm-boinc/batch-inventory.py \
 && command -v usnm-boinc
