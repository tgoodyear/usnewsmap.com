# Security updates only, on the exact running image: the same upgrade and PHP floor as the
# server and bridge images, and no change to the project's code or files. This is the quick
# rollout after a Debian security advisory (docs/boinc-onprem.md, "Security updates"). There
# are no COPYs, so any directory is a context:
#
#   podman build --build-arg RUNTIME_IMAGE=<running image> --build-arg SECURITY_REFRESH=<date> \
#     -t usnm-boinc-bridge:<running tag>-security \
#     -f contrib-compute/images/server/security-overlay.Containerfile contrib-compute/images/server
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
