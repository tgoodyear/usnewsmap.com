# usnm-api container image (08 §8.6): static-ish Rust binary on distroless,
# non-root, serving the API and the built web app (ADR-0009).
FROM node:24-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web ./
# Same origin as the API: VITE_API_BASE stays empty.
RUN npm run build && node scripts/precompress.mjs dist

# Dependencies build in their own layer (cargo-chef), keyed on the manifests
# and lockfile only, so a code change recompiles just the workspace crates.
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src

FROM chef AS planner
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY rust-toolchain.toml ./
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p usnm-api --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# The home page's example searches, which the API warms (include_str!).
COPY web/src/examples.json ./web/src/examples.json
# The reconstructed build records of early index versions, for /v1/versions (include_str!).
COPY ops/index-history.json ./ops/index-history.json
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
