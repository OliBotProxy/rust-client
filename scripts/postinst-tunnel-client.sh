#!/bin/bash
set -e

mkdir -p /etc/tunnel-client

if [ ! -f /etc/tunnel-client/env ]; then
    cat > /etc/tunnel-client/env <<'EOF'
# Oli.bot Tunnel Client configuration
# Edit this file, then run: sudo systemctl start tunnel-client

# API endpoint — choose your region:
#   https://api-us.oli.bot/api   (United States)
#   https://api-eu.oli.bot/api   (Europe)
#   https://api-asia.oli.bot/api (Asia)
TUNNEL_API_URL=https://api-us.oli.bot/api

# Your tunnel ID from the oli.bot dashboard
TUNNEL_ID=YOUR_TUNNEL_ID

# Your API key (<subscriptionId>_<salt> format)
TUNNEL_API_KEY=YOUR_API_KEY
EOF
    chmod 600 /etc/tunnel-client/env
fi

systemctl daemon-reload
systemctl enable tunnel-client.service

if grep -q "YOUR_TUNNEL_ID" /etc/tunnel-client/env 2>/dev/null; then
    echo ""
    echo "=== Oli.bot Tunnel Client installed ==="
    echo "Edit /etc/tunnel-client/env with your credentials, then run:"
    echo "  sudo systemctl start tunnel-client"
    echo ""
else
    systemctl restart tunnel-client.service || true
fi
