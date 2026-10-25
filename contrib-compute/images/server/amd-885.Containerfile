# Scoped release on the exact tls464-rb3 live runtime, not current main's broader
# server/bridge. Only the reviewed AMD policy, staging tool and operator adapters
# change. Scheduler, collector, config/TLS code, credentials and validators stay put.
ARG STAGING_IMAGE
ARG RUNTIME_IMAGE=crusnmprodffapzy.azurecr.io/usnm-boinc-bridge@sha256:4cd42232b18d9a7b6317da4be490d50ca569de82c6ca6b935784fe173c896201
FROM ${STAGING_IMAGE} AS staging
FROM ${RUNTIME_IMAGE}
# Debian security updates at build time (#969): the image this builds on is pinned, so its
# packages are frozen at whatever it was built with. A new SECURITY_REFRESH (a date) makes a
# cached build run this layer again. PHP_FLOOR and APACHE_FLOOR are the oldest php8.2 and
# apache2 the image may ship: DSA-6255-1 (CVE-2026-109618, CVE-2026-109619, 21 October 2026)
# and DSA-6323-1 (CVE-2026-49975, 6 June 2026). The build fails below either; raise it after
# the next PHP or Apache DSA, here, in the other Containerfiles and in
# scripts/boinc/onprem/usnm-image-security-check.service.
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
COPY --from=staging /usr/local/boinc/tools/create_work /usr/local/boinc/tools/create_work
COPY contrib-compute/boinc/plan_class_spec.xml contrib-compute/images/server/refresh-plan-policy.py /usr/local/lib/usnm-boinc/
COPY scripts/boinc/acceptance_server.py scripts/boinc/native-qwen-cohort.php /usr/local/lib/usnm-boinc/amd-885/
RUN mv /usr/local/lib/usnm-boinc/entrypoint.sh /usr/local/lib/usnm-boinc/entrypoint-before-amd-885.sh
COPY contrib-compute/images/server/amd-885-entrypoint.sh /usr/local/lib/usnm-boinc/entrypoint.sh
RUN chmod 0755 /usr/local/lib/usnm-boinc/entrypoint.sh \
 && ldd /usr/local/boinc/tools/create_work > /usr/local/lib/usnm-boinc/amd-885/create-work-ldd.txt \
 && ! grep -q 'not found' /usr/local/lib/usnm-boinc/amd-885/create-work-ldd.txt \
 && /usr/local/boinc/tools/create_work --help 2>&1 | grep -q -- '--no_new_results'
