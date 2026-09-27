# usnm-api container image (08 §8.6): static-ish Rust binary on distroless, non-root.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --locked -p usnm-api

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/usnm-api /usr/local/bin/usnm-api
# Synthetic fixtures let the image start without cloud data (USNM_BACKEND=memory).
COPY fixtures/data /srv/fixtures
ENV USNM_BIND=0.0.0.0:8080 \
    USNM_DATA_DIR=/srv/fixtures \
    USNM_BACKEND=memory
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/usnm-api"]
