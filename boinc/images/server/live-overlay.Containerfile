# Scoped release on the exact live web image (the AMD 885 overlay, d417a2ad…), not current
# main's broader server/bridge. It adds only what main has changed in the image's own files
# since that build: NVIDIA general admission that survives a policy refresh (#909) and the
# stop_maintenance hold in the daemon wrapper (#921), and the pipeline dashboard
# (pipeline.html, pipeline_summary.php; the entrypoint copies html/ into the project at
# start). Build it from a context holding just these five files at their repository paths:
#
#   az acr build --registry <registry> --image usnm-boinc-bridge:<commit>-live-overlay \
#     --file boinc/images/server/live-overlay.Containerfile <context>
ARG RUNTIME_IMAGE=crusnmprodffapzy.azurecr.io/usnm-boinc-bridge@sha256:d417a2ad0469c2b0c54787a0e466d6e76cb78577e40a1e776870bef252b4da9e
FROM ${RUNTIME_IMAGE}
# Debian security updates at build time (#969): the image this builds on is pinned, so its
# packages are frozen at whatever it was built with. A new SECURITY_REFRESH (a date) makes a
# cached build run this layer again. PHP_FLOOR and APACHE_FLOOR are the oldest php8.2 and
# apache2 the image may ship: DLA-4848-1 (CVE-2026-109618, CVE-2026-109619, 21 October 2026)
# and DSA-6323-1 (CVE-2026-49975, 6 June 2026). The build fails below either; raise it after
# the next PHP or Apache advisory, here, in the other Containerfiles and in
# boinc/scripts/onprem/usnm-image-security-check.service.
ARG SECURITY_REFRESH=2026-10-21
ARG PHP_FLOOR=8.2.35-1~deb12u1
ARG APACHE_FLOOR=2.4.67-1~deb12u3
RUN echo "security refresh $SECURITY_REFRESH" \
 && apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get -y -o Dpkg::Options::=--force-confold upgrade \
 && rm -rf /var/lib/apt/lists/* \
 && php_version="$(dpkg-query -W -f='${Version}' php8.2-common)" \
 && if ! dpkg --compare-versions "$php_version" ge "$PHP_FLOOR"; then \
      echo "php8.2-common $php_version is older than $PHP_FLOOR" >&2; exit 1; fi \
 && apache_version="$(dpkg-query -W -f='${Version}' apache2)" \
 && if ! dpkg --compare-versions "$apache_version" ge "$APACHE_FLOOR"; then \
      echo "apache2 $apache_version is older than $APACHE_FLOOR" >&2; exit 1; fi
COPY boinc/images/server/refresh-plan-policy.py boinc/images/server/boinc-daemons.sh /usr/local/lib/usnm-boinc/
COPY boinc/scripts/native-qwen-cohort.php /usr/local/lib/usnm-boinc/amd-885/
COPY boinc/project/html/user/pipeline.html boinc/project/html/user/pipeline_summary.php /usr/local/lib/usnm-boinc/html/user/
RUN chmod 0755 /usr/local/lib/usnm-boinc/boinc-daemons.sh \
 && grep -q 'usnm-nvidia-general-admission-v1' /usr/local/lib/usnm-boinc/refresh-plan-policy.py \
 && grep -q 'usnm-nvidia-general-admission-v1' /usr/local/lib/usnm-boinc/amd-885/native-qwen-cohort.php \
 && grep -q 'stop_maintenance' /usr/local/lib/usnm-boinc/boinc-daemons.sh \
 && php -l /usr/local/lib/usnm-boinc/amd-885/native-qwen-cohort.php \
 && python3 -m py_compile /usr/local/lib/usnm-boinc/refresh-plan-policy.py \
 && bash -n /usr/local/lib/usnm-boinc/boinc-daemons.sh \
 && php -l /usr/local/lib/usnm-boinc/html/user/pipeline_summary.php \
 && grep -q 'pipeline_summary.php' /usr/local/lib/usnm-boinc/html/user/pipeline.html
