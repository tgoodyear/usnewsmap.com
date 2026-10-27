## CI-only adapter used to exercise the worker against a real Podman
## container/cgroup without representing a production OCR engine.
ARG BASE_IMAGE=localhost/usnm-ocr/tesseract:check
FROM ${BASE_IMAGE}
COPY contract-test-adapter.sh /usr/local/bin/contract-test-adapter.sh
ENTRYPOINT ["/bin/sh", "/usr/local/bin/contract-test-adapter.sh"]
