# usnm-api container image (08 §8.6): static-ish Rust binary on distroless,
# non-root, serving the API and the built web app (ADR-0009).
FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web ./
# Same origin as the API: VITE_API_BASE stays empty.
RUN npm run build && node scripts/precompress.mjs dist

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --locked -p usnm-api

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/usnm-api /usr/local/bin/usnm-api
COPY --from=web /web/dist /srv/site
# Synthetic fixtures let the image start without cloud data (USNM_BACKEND=memory).
COPY fixtures/data /srv/fixtures
ENV USNM_BIND=0.0.0.0:8080 \
    USNM_DATA_DIR=/srv/fixtures \
    USNM_BACKEND=memory \
    USNM_SITE_DIR=/srv/site
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/usnm-api"]
