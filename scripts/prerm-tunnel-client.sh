#!/bin/bash
set -e
systemctl stop tunnel-client.service 2>/dev/null || true
systemctl disable tunnel-client.service 2>/dev/null || true
