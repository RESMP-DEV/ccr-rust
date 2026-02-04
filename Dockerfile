# Build stage
FROM rust:1.75-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN useradd -r -u 1000 ccr
COPY --from=builder /app/target/release/ccr-rust /usr/local/bin/
USER ccr
EXPOSE 3456
HEALTHCHECK --interval=30s --timeout=5s CMD curl -f http://localhost:3456/health || exit 1
ENTRYPOINT ["ccr-rust"]
CMD ["start", "--host", "0.0.0.0"]