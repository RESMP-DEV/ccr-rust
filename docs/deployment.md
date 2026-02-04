# Deploying CCR-Rust

## Building

```bash
cargo build --release
# Binary at target/release/ccr-rust
```

## Systemd (Linux)

```ini
[Unit]
Description=Claude Code Router (Rust)
After=network.target

[Service]
Type=simple
User=ccr
ExecStart=/usr/local/bin/ccr-rust --config /etc/ccr/config.json
Restart=always
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

## Docker

```dockerfile
FROM rust:1.75 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/ccr-rust /usr/local/bin/
ENTRYPOINT ["ccr-rust"]
```

## Kubernetes

See `k8s/` directory for manifests.

## Security

- Run as non-root user
- Store API keys in secrets, not config files
- Use TLS termination at load balancer
