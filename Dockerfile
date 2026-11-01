FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src

FROM build AS test
RUN cargo test --locked

FROM build AS binary
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=binary /src/target/release/rust-api /api
ENTRYPOINT ["/api"]
