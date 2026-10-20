# Scoped release on the exact live web image (the AMD 885 overlay, d417a2ad…), not current
# main's broader server/bridge. It adds only what main has changed in the image's own files
# since that build: NVIDIA general admission that survives a policy refresh (#909) and the
# stop_maintenance hold in the daemon wrapper (#921). Build it from a context holding just
# these three files at their repository paths:
#
#   az acr build --registry <registry> --image usnm-boinc-bridge:<commit>-live-overlay \
#     --file contrib-compute/images/server/live-overlay.Containerfile <context>
ARG RUNTIME_IMAGE=crusnmprodffapzy.azurecr.io/usnm-boinc-bridge@sha256:d417a2ad0469c2b0c54787a0e466d6e76cb78577e40a1e776870bef252b4da9e
FROM ${RUNTIME_IMAGE}
COPY contrib-compute/images/server/refresh-plan-policy.py contrib-compute/images/server/boinc-daemons.sh /usr/local/lib/usnm-boinc/
COPY scripts/boinc/native-qwen-cohort.php /usr/local/lib/usnm-boinc/amd-885/
RUN chmod 0755 /usr/local/lib/usnm-boinc/boinc-daemons.sh \
 && grep -q 'usnm-nvidia-general-admission-v1' /usr/local/lib/usnm-boinc/refresh-plan-policy.py \
 && grep -q 'usnm-nvidia-general-admission-v1' /usr/local/lib/usnm-boinc/amd-885/native-qwen-cohort.php \
 && grep -q 'stop_maintenance' /usr/local/lib/usnm-boinc/boinc-daemons.sh \
 && php -l /usr/local/lib/usnm-boinc/amd-885/native-qwen-cohort.php \
 && python3 -m py_compile /usr/local/lib/usnm-boinc/refresh-plan-policy.py \
 && bash -n /usr/local/lib/usnm-boinc/boinc-daemons.sh
