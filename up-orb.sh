#!/usr/bin/env bash
# Insight platform — bring up services on OrbStack K8s.
#
# Components:
#   1. Backend  (Analytics API, API Gateway)
#   2. Frontend (SPA)
#
# Prerequisites:
#   - OrbStack with K8s enabled
#   - kubectl, helm, docker
#   - MariaDB, ClickHouse, Redis, Redpanda already running in cluster
#     (deploy via: helmfile -e local -l tier=infra sync)
#
# Usage:
#   ./up-orb.sh              # full stack
#   ./up-orb.sh backend      # only backend services
#   ./up-orb.sh frontend     # only frontend
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

COMPONENT="${1:-all}"
NAMESPACE="insight"

# ─── Okta OIDC (set via .env.local or environment) ────────────────────────
ENV_FILE="$ROOT_DIR/.env.local"
if [[ -f "$ENV_FILE" ]]; then
  # shellcheck source=/dev/null
  source "$ENV_FILE"
fi

: "${OIDC_ISSUER:?ERROR: OIDC_ISSUER is required — set it in .env.local}"
: "${OIDC_CLIENT_ID:?ERROR: OIDC_CLIENT_ID is required — set it in .env.local}"
OIDC_REDIRECT_URI="${OIDC_REDIRECT_URI:-http://localhost:9999/callback}"
OIDC_AUDIENCE="${OIDC_AUDIENCE:-$OIDC_CLIENT_ID}"

# Timestamp to force pod restarts when image tag doesn't change
DEPLOY_TS="$(date +%s)"

echo "=== Insight Platform — OrbStack ==="

# --- Prerequisites ---
for cmd in kubectl helm docker; do
  if ! command -v "$cmd" &>/dev/null; then
    echo "ERROR: $cmd is required but not found" >&2
    exit 1
  fi
done

# Verify cluster is reachable
if ! kubectl cluster-info &>/dev/null; then
  echo "ERROR: K8s cluster not reachable. Is OrbStack running?" >&2
  exit 1
fi

# ─── Namespace ──────────────────────────────────────────────────────────────
kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

# ─── Backend ────────────────────────────────────────────────────────────────
if [[ "$COMPONENT" == "all" || "$COMPONENT" == "backend" ]]; then
  echo "=== Building Analytics API ==="
  docker build -t insight-analytics-api:local \
    -f src/backend/services/analytics-api/Dockerfile \
    src/backend/

  echo "=== Deploying Analytics API ==="
  helm upgrade --install insight-analytics src/backend/services/analytics-api/helm/ \
    --namespace "$NAMESPACE" \
    --set image.repository=insight-analytics-api \
    --set image.tag=local \
    --set image.pullPolicy=IfNotPresent \
    --set database.url="mysql://insight:insight-local@mariadb:3306/analytics" \
    --set clickhouse.url="http://clickhouse:8123" \
    --set clickhouse.database=insight \
    --set clickhouse.user=insight \
    --set clickhouse.password=insight-local \
    --set redis.url="redis://redis-master:6379" \
    --set identityResolution.url="" \
    --set-string podAnnotations.deployedAt="$DEPLOY_TS" \
    --wait --timeout 3m

  echo "=== Building API Gateway ==="
  docker build -t insight-api-gateway:local \
    -f src/backend/services/api-gateway/Dockerfile \
    src/backend/

  echo "=== Deploying API Gateway ==="
  helm upgrade --install insight-gw src/backend/services/api-gateway/helm/ \
    --namespace "$NAMESPACE" \
    --set image.repository=insight-api-gateway \
    --set image.tag=local \
    --set image.pullPolicy=IfNotPresent \
    --set authDisabled=false \
    --set ingress.enabled=false \
    --set gateway.enableDocs=true \
    --set oidc.issuer="$OIDC_ISSUER" \
    --set oidc.audience="$OIDC_AUDIENCE" \
    --set oidc.clientId="$OIDC_CLIENT_ID" \
    --set oidc.redirectUri="$OIDC_REDIRECT_URI" \
    --set proxy.routes[0].prefix=/analytics \
    --set proxy.routes[0].upstream=http://insight-analytics-analytics-api:8081 \
    --set proxy.routes[0].public=false \
    --set-string podAnnotations.deployedAt="$DEPLOY_TS" \
    --wait --timeout 3m
fi

# ─── Frontend ───────────────────────────────────────────────────────────────
if [[ "$COMPONENT" == "all" || "$COMPONENT" == "frontend" ]]; then
  FRONTEND_DIR="$ROOT_DIR/../insight-front"
  if [[ ! -d "$FRONTEND_DIR" ]]; then
    echo "WARNING: Frontend directory not found at $FRONTEND_DIR — skipping"
  else
    echo "=== Building Frontend ==="
    docker build -t insight-frontend:local \
      -f src/frontend/Dockerfile \
      "$FRONTEND_DIR"

    echo "=== Deploying Frontend ==="
    helm upgrade --install insight-fe src/frontend/helm/ \
      --namespace "$NAMESPACE" \
      --set image.repository=insight-frontend \
      --set image.tag=local \
      --set image.pullPolicy=IfNotPresent \
      --set ingress.enabled=false \
      --set oidc.issuer="$OIDC_ISSUER" \
      --set oidc.clientId="$OIDC_CLIENT_ID" \
      --set-string podAnnotations.deployedAt="$DEPLOY_TS" \
      --wait --timeout 3m
  fi
fi

# ─── Ingress ───────────────────────────────────────────────────────────────
echo "=== Applying Ingress ==="
kubectl apply -n "$NAMESPACE" -f - <<'EOF'
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: insight
  annotations:
    nginx.ingress.kubernetes.io/proxy-buffer-size: "8k"
spec:
  ingressClassName: nginx
  rules:
    - http:
        paths:
          - path: /api
            pathType: Prefix
            backend:
              service:
                name: insight-gw-api-gateway
                port:
                  number: 8080
          - path: /health
            pathType: Prefix
            backend:
              service:
                name: insight-gw-api-gateway
                port:
                  number: 8080
          - path: /
            pathType: Prefix
            backend:
              service:
                name: insight-fe
                port:
                  number: 80
EOF

# ─── Port-forward ──────────────────────────────────────────────────────────
# Kill any existing port-forward on 9999
lsof -ti :9999 | xargs kill -9 2>/dev/null || true

echo "=== Starting port-forward on :9999 ==="
kubectl port-forward -n ingress-nginx svc/ingress-nginx-controller 9999:80 &>/dev/null &
PF_PID=$!
sleep 1

if kill -0 "$PF_PID" 2>/dev/null; then
  echo "  Port-forward PID: $PF_PID"
else
  echo "WARNING: port-forward failed. Try manually:"
  echo "  kubectl port-forward -n ingress-nginx svc/ingress-nginx-controller 9999:80"
fi

# ─── Summary ────────────────────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════"
echo "  Namespace:    $NAMESPACE"
echo "  Auth:         Okta ($OIDC_ISSUER)"
echo "  Redirect URI: $OIDC_REDIRECT_URI"
echo ""
echo "  http://localhost:9999            — Frontend + API (via ingress)"
echo "  http://localhost:9999/api/...    — API Gateway"
echo "  http://localhost:9999/health     — Health check"
echo ""
echo "  Stop port-forward: kill $PF_PID"
echo "════════════════════════════════════════════════"
