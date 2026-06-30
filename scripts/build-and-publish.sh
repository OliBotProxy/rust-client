#!/bin/bash
set -e

cd "$(dirname "$0")/.."
BUILD_DIR="$(pwd)"

export PATH="/opt/homebrew/opt/binutils/bin:/opt/homebrew/opt/gnu-tar/libexec/gnubin:$PATH"

FPM="$HOME/.gem/ruby/2.6.0/bin/fpm"
if [[ ! -x "$FPM" ]]; then
    echo "❌ Ruby fpm gem not found at $FPM"
    echo "   Install with: gem install fpm --user-install"
    exit 1
fi

VERSION="0.9.4"
PROJECT="proxyconfig-71f70"
LOCATION="us-central1"

for cmd in cross gcloud; do
    if ! command -v $cmd &> /dev/null; then
        echo "❌ Error: $cmd is not installed."
        exit 1
    fi
done

TARGETS=(
    "x86_64-unknown-linux-musl:amd64:x86_64-linux-musl-"
    "aarch64-unknown-linux-musl:arm64:aarch64-linux-musl-"
)

echo "🎯 Ensuring Rust targets are installed..."
for target_info in "${TARGETS[@]}"; do
    IFS=':' read -r TARGET ARCH _COMPILER_PREFIX <<< "$target_info"
    rustup target add "$TARGET" || true
done

for target_info in "${TARGETS[@]}"; do
    IFS=':' read -r TARGET ARCH COMPILER_PREFIX <<< "$target_info"
    echo "========================================================"
    echo "🛠️  Building tunnel-client for $TARGET ($ARCH)..."
    echo "========================================================"

    UPPER_TARGET=$(echo "$TARGET" | tr '[:lower:]-' '[:upper:]_')
    UNDERSCORE_TARGET=$(echo "$TARGET" | tr '-' '_')
    export "CARGO_TARGET_${UPPER_TARGET}_LINKER"="${COMPILER_PREFIX}gcc"
    export "CC_${UNDERSCORE_TARGET}"="${COMPILER_PREFIX}gcc"

    cargo build --bin tunnel-client --target "$TARGET" --release

    echo "📦 Stripping binary..."
    "${COMPILER_PREFIX}strip" --strip-all "target/$TARGET/release/tunnel-client" 2>/dev/null || true

    mkdir -p "pkg-${ARCH}/usr/local/bin" "pkg-${ARCH}/lib/systemd/system"
    cp "target/$TARGET/release/tunnel-client" "pkg-${ARCH}/usr/local/bin/"
    cp "scripts/tunnel-client.service"        "pkg-${ARCH}/lib/systemd/system/"
    chmod 755 "pkg-${ARCH}/usr/local/bin/tunnel-client"

    DEB_NAME="$BUILD_DIR/tunnel-client_${VERSION}_${ARCH}.deb"
    RPM_NAME="$BUILD_DIR/tunnel-client-${VERSION}-1.${ARCH}.rpm"

    $FPM -s dir -t deb -n tunnel-client -v "$VERSION" \
        --architecture "$ARCH" \
        --depends "ca-certificates" \
        --maintainer "Oli.bot <info@oli.bot>" \
        --description "Oli.bot tunnel client — expose local backends through a secure reverse-proxy tunnel" \
        --after-install "scripts/postinst-tunnel-client.sh" \
        --before-remove  "scripts/prerm-tunnel-client.sh" \
        -p "$DEB_NAME" \
        -C "pkg-${ARCH}" \
        usr/local/bin/tunnel-client \
        lib/systemd/system/tunnel-client.service

    $FPM -s dir -t rpm -n tunnel-client -v "$VERSION" \
        --architecture "$ARCH" \
        --depends "ca-certificates" \
        --maintainer "Oli.bot <info@oli.bot>" \
        --description "Oli.bot tunnel client — expose local backends through a secure reverse-proxy tunnel" \
        --after-install "scripts/postinst-tunnel-client.sh" \
        --before-remove  "scripts/prerm-tunnel-client.sh" \
        -p "$RPM_NAME" \
        -C "pkg-${ARCH}" \
        usr/local/bin/tunnel-client \
        lib/systemd/system/tunnel-client.service

    rm -rf "pkg-${ARCH}"
    echo "✅ Done: $ARCH"
done

echo "========================================================"
echo "☁️  Pushing to Google Artifact Registry ($LOCATION)..."
echo "========================================================"

gcloud config set project "$PROJECT" --quiet

for target_info in "${TARGETS[@]}"; do
    IFS=':' read -r _TARGET ARCH _COMPILER_PREFIX <<< "$target_info"
    DEB="$BUILD_DIR/tunnel-client_${VERSION}_${ARCH}.deb"
    RPM="$BUILD_DIR/tunnel-client-${VERSION}-1.${ARCH}.rpm"

    echo "⬆️  $(basename "$DEB") → tunnel-client-apt"
    gcloud artifacts apt upload tunnel-client-apt --location="$LOCATION" --source="$DEB"

    echo "⬆️  $(basename "$RPM") → tunnel-client-yum"
    gcloud artifacts yum upload tunnel-client-yum --location="$LOCATION" --source="$RPM"
done

echo "========================================================"
echo "🎉 tunnel-client $VERSION built and published!"
echo "========================================================"
