#!/bin/bash
set -e

mkdir -p /etc/tunnel-client

if [ ! -f /etc/tunnel-client/env ]; then
    cat > /etc/tunnel-client/env <<'EOF'
# clientproxy.io Tunnel Client configuration
# Edit this file, then run: sudo systemctl start tunnel-client

# API endpoint — choose your region:
#   https://api-us.clientproxy.io/api   (United States)
#   https://api-eu.clientproxy.io/api   (Europe)
#   https://api-asia.clientproxy.io/api (Asia)
TUNNEL_API_URL=https://api-us.clientproxy.io/api

# Your tunnel ID from the clientproxy.io dashboard
TUNNEL_ID=YOUR_TUNNEL_ID

# Your API key (<subscriptionId>_<salt> format)
TUNNEL_API_KEY=YOUR_API_KEY
EOF
    chmod 600 /etc/tunnel-client/env
fi

# Preserve credentials while moving an existing default regional API URL.
for region in us eu asia; do
    old_url="https://api-${region}.oli.bot/api"
    new_url="https://api-${region}.clientproxy.io/api"
    if grep -Fxq "TUNNEL_API_URL=${old_url}" /etc/tunnel-client/env; then
        sed -i "s|^TUNNEL_API_URL=${old_url}$|TUNNEL_API_URL=${new_url}|" /etc/tunnel-client/env
        break
    fi
done

systemctl daemon-reload
systemctl enable tunnel-client.service

if grep -q "YOUR_TUNNEL_ID" /etc/tunnel-client/env 2>/dev/null; then
    echo ""
    echo "=== clientproxy.io Tunnel Client installed ==="
    echo "Edit /etc/tunnel-client/env with your credentials, then run:"
    echo "  sudo systemctl start tunnel-client"
    echo ""
else
    systemctl restart tunnel-client.service || true
fi
