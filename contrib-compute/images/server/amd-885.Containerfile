# Scoped release on the exact tls464-rb3 live runtime, not current main's broader
# server/bridge. Only the reviewed AMD policy, staging tool and operator adapters
# change. Scheduler, collector, config/TLS code, credentials and validators stay put.
ARG STAGING_IMAGE
ARG RUNTIME_IMAGE=crusnmprodffapzy.azurecr.io/usnm-boinc-bridge@sha256:4cd42232b18d9a7b6317da4be490d50ca569de82c6ca6b935784fe173c896201
FROM ${STAGING_IMAGE} AS staging
FROM ${RUNTIME_IMAGE}
COPY --from=staging /usr/local/boinc/tools/create_work /usr/local/boinc/tools/create_work
COPY contrib-compute/boinc/plan_class_spec.xml contrib-compute/images/server/refresh-plan-policy.py /usr/local/lib/usnm-boinc/
COPY scripts/boinc/acceptance_server.py scripts/boinc/native-qwen-cohort.php /usr/local/lib/usnm-boinc/amd-885/
RUN mv /usr/local/lib/usnm-boinc/entrypoint.sh /usr/local/lib/usnm-boinc/entrypoint-before-amd-885.sh
COPY contrib-compute/images/server/amd-885-entrypoint.sh /usr/local/lib/usnm-boinc/entrypoint.sh
RUN chmod 0755 /usr/local/lib/usnm-boinc/entrypoint.sh \
 && ldd /usr/local/boinc/tools/create_work > /usr/local/lib/usnm-boinc/amd-885/create-work-ldd.txt \
 && ! grep -q 'not found' /usr/local/lib/usnm-boinc/amd-885/create-work-ldd.txt \
 && /usr/local/boinc/tools/create_work --help 2>&1 | grep -q -- '--no_new_results'
