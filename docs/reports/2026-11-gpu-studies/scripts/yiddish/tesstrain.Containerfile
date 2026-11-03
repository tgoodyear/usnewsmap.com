# Study 10's trainer: the pinned usnm_ocr Tesseract image (Debian 5.5.0-1+b1, which ships lstmtraining
# and the other training tools) plus make, Python, Pillow and python-bidi for tesstrain, from the
# image's own snapshot.debian.org pin.
FROM localhost/usnm-tesseract:main-cf07f950
USER root
RUN apt-get -o Acquire::Check-Valid-Until=false update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends make python3 python3-pil python3-bidi wget ca-certificates \
 && rm -rf /var/lib/apt/lists/*
ENTRYPOINT []
