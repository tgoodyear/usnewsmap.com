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
# Debian security updates at build time (#969): the image this builds on is pinned, so its
# packages are frozen at whatever it was built with. A new SECURITY_REFRESH (a date) makes a
# cached build run this layer again. PHP_FLOOR is the oldest php8.2 the image may ship:
# DSA-6255-1 (CVE-2026-109618, CVE-2026-109619, 21 October 2026). The build fails below it;
# raise it after the next PHP DSA.
ARG SECURITY_REFRESH=2026-10-21
ARG PHP_FLOOR=8.2.35-1~deb12u1
RUN echo "security refresh $SECURITY_REFRESH" \
 && apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get -y -o Dpkg::Options::=--force-confold upgrade \
 && rm -rf /var/lib/apt/lists/* \
 && php_version="$(dpkg-query -W -f='${Version}' php8.2-common)" \
 && if ! dpkg --compare-versions "$php_version" ge "$PHP_FLOOR"; then \
      echo "php8.2-common $php_version is older than $PHP_FLOOR" >&2; exit 1; fi
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
