#!/usr/bin/env bash
# bench-network.sh — time the GHCR + GitHub round-trips the changelog fetch
# actually makes. Usage:
#
#   build-aux/bench-network.sh ghcr.io/ublue-os/bluefin:stable
#   build-aux/bench-network.sh ghcr.io/projectbluefin/dakota:latest
#
# Prints, in order:
#   anon token            (fetch a pull-scope bearer)
#   tag list              (full /tags/list, count tags)
#   manifest HEADs        (16 parallel HEAD on most-recent dated tags)
#   SPDX referrers        (OCI 1.1 /referrers lookup for the booted manifest)
#
# Used to triage where the changelog page's perceived "freeze" is actually
# coming from — usually it's the SBOM blob pull + JSON parse, which is
# downstream of these probes.

set -uo pipefail
# Intentionally NOT set -e — we want individual probes to fail gracefully
# (referrers API is optional on GHCR; tag list / token / manifest probes are
# the real signal) without aborting the whole benchmark.

REF="${1:-}"
if [ -z "$REF" ]; then
    echo "usage: $0 <ref>  (e.g. ghcr.io/ublue-os/bluefin:stable)" >&2
    exit 2
fi

HOST="$(echo "$REF" | cut -d/ -f1)"
ORG="$(echo "$REF" | cut -d/ -f2)"
REST="$(echo "$REF" | cut -d/ -f3-)"
IMAGE="$(echo "$REST" | cut -d: -f1)"
TAG="$(echo "$REST" | cut -d: -f2)"
[ "$TAG" = "$IMAGE" ] && TAG=latest

echo "▶ benchmarking $HOST/$ORG/$IMAGE:$TAG"
echo

# 1. Anon token
T1=$(date +%s%N)
TOKEN=$(curl -sS "https://$HOST/token?scope=repository:$ORG/$IMAGE:pull" \
        | python3 -c 'import sys,json; print(json.load(sys.stdin)["token"])')
T2=$(date +%s%N)
printf "  %-32s %6.0f ms\n" "anon token" "$(( (T2-T1) / 1000000 ))"

# 2. Full tag list
T1=$(date +%s%N)
TAGS=$(curl -sS -H "Authorization: Bearer $TOKEN" \
       "https://$HOST/v2/$ORG/$IMAGE/tags/list")
T2=$(date +%s%N)
N_TAGS=$(echo "$TAGS" | python3 -c 'import sys,json; print(len(json.load(sys.stdin)["tags"]))')
printf "  %-32s %6.0f ms  (%s tags)\n" "tag list" "$(( (T2-T1) / 1000000 ))" "$N_TAGS"

# 3. 16 concurrent manifest HEADs against the most-recent dated tags
SAMPLE=$(echo "$TAGS" | python3 -c '
import sys, json, re
d = json.load(sys.stdin)
dated = [t for t in d["tags"] if re.search(r"\d{8}", t) and "sha256" not in t]
print("\n".join(sorted(dated, reverse=True)[:16]))
')
N_PROBE=$(echo "$SAMPLE" | wc -l)
T1=$(date +%s%N)
echo "$SAMPLE" \
    | xargs -P 16 -n 1 -I {} \
        curl -sS -o /dev/null -I \
            -H "Authorization: Bearer $TOKEN" \
            "https://$HOST/v2/$ORG/$IMAGE/manifests/{}"
T2=$(date +%s%N)
printf "  %-32s %6.0f ms  (%s parallel)\n" \
    "manifest HEADs" "$(( (T2-T1) / 1000000 ))" "$N_PROBE"

# 4. SPDX referrer lookup for the booted ref
DIGEST=$(curl -sS \
    -H "Authorization: Bearer $TOKEN" \
    -H "Accept: application/vnd.oci.image.manifest.v1+json" \
    -I "https://$HOST/v2/$ORG/$IMAGE/manifests/$TAG" \
    | awk '/^docker-content-digest:/ {print $2}' \
    | tr -d '\r')
if [ -n "$DIGEST" ]; then
    T1=$(date +%s%N)
    REFERRERS=$(curl -sS -H "Authorization: Bearer $TOKEN" \
        "https://$HOST/v2/$ORG/$IMAGE/referrers/$DIGEST?artifactType=application/spdx%2Bjson" \
        || echo "")
    T2=$(date +%s%N)
    N_REFS=$(echo "$REFERRERS" \
        | python3 -c 'import sys,json; print(len(json.load(sys.stdin).get("manifests",[])))' 2>/dev/null \
        || echo "n/a")
    printf "  %-32s %6.0f ms  (%s artifacts)\n" \
        "SPDX referrers" "$(( (T2-T1) / 1000000 ))" "$N_REFS"
else
    printf "  %-32s        (skipped — no digest for $TAG)\n" "SPDX referrers"
fi

echo
echo "tip: the SBOM blob pull + JSON parse is the typical freeze culprit;"
echo "     run it via tokio::spawn so it doesn't block the changelog flow."
