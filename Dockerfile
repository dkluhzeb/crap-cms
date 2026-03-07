# Build stage
FROM rust:1.86-bookworm AS builder

WORKDIR /app

# protoc for tonic-build
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

# Copy source
COPY . .

# Build release binary
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

# ca-certificates for outbound TLS (e.g. SMTP, HTTP client)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/crap-cms /usr/local/bin/crap-cms

VOLUME ["/config"]

EXPOSE 3000
EXPOSE 50051

ENTRYPOINT ["crap-cms"]
CMD ["serve", "/config"]
