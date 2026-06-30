# ── Build stage ───────────────────────────────────────────────────────────────
FROM rust:1.91-slim-bookworm AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        curl \
    && rm -rf /var/lib/apt/lists/*

# Path dependency — checked out as a sibling in CI and copied here.
COPY ullav-mcp-auth /ullav-mcp-auth

# Cache dependencies before copying source
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs
RUN cargo build --release
RUN rm -f target/release/deps/ullav_dam_server*

# Build real binary
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release

# Download prebuilt PDFium shared library (runtime-loaded by pdfium-render)
# Pin to a specific chromium build for reproducibility if needed.
RUN mkdir -p /tmp/pdfium \
    && curl -fsSL https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-linux-x64.tgz \
       | tar -xz -C /tmp/pdfium \
    && mv /tmp/pdfium/lib/libpdfium.so /app/libpdfium.so

# ── Runtime stage ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

# ca-certificates: TLS for S3/MinIO
# libreoffice: headless Office-to-PDF conversion for thumbnails
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libreoffice \
    && rm -rf /var/lib/apt/lists/*

# Non-root user; LibreOffice writes a profile to $HOME
RUN useradd -m -u 1001 dam
WORKDIR /app

COPY --from=builder /app/target/release/ullav-dam-server ./ullav-dam-server
COPY --from=builder /app/migrations ./migrations
COPY --from=builder /app/libpdfium.so ./libpdfium.so

ENV PDFIUM_LIB_PATH=/app/libpdfium.so
ENV SOFFICE_PATH=/usr/bin/soffice

USER dam
EXPOSE 8080

CMD ["./ullav-dam-server"]
