# Layer 1: Builder
FROM rust:1.86-alpine AS builder
WORKDIR /usr/src/app

# Install build dependencies
RUN apk add --no-cache pkgconfig openssl-dev musl-dev openssl-libs-static postgresql-dev bash g++ make

# Copy the source code and build the application
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --target x86_64-unknown-linux-musl

# Layer 2: Runtime
FROM alpine:3.19 AS runtime
WORKDIR /app

# Install runtime dependencies
RUN apk add --no-cache ca-certificates

# Copy the binary from builder
COPY --from=builder /usr/src/app/target/x86_64-unknown-linux-musl/release/main /app/main

CMD ["/app/main"]
