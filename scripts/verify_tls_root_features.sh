#!/usr/bin/env bash
#
# Guards the TLS trust configuration from cold-start hypothesis H1 (PR #1276).
#
# The non-FIPS (default) build must trust the compiled-in webpki root bundle so
# it does NOT run rustls_native_certs::load_native_certs() — a per-build
# filesystem cert scan — on the cold-start critical path. The FIPS build must
# keep native roots. Cargo unifies features additively across the dependency
# graph, so a dependency added later could silently flip reqwest's root source
# (re-introducing the scan, or cross-linking webpki into FIPS). This catches
# that before it ships. Errors are handled explicitly (no `set -e`) so a
# non-matching grep can't masquerade as a build failure.
set -uo pipefail

cd "$(dirname "$0")/../bottlecap" || exit 2

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

# Activated feature list for reqwest under a given bottlecap feature set.
reqwest_feature_line() { # $1 = bottlecap feature set
    cargo tree --no-default-features --features "$1" -i reqwest -f '{p}|{f}' 2>/dev/null \
        | grep -m1 '^reqwest ' | sed 's/^[^|]*|//'
}

echo "[tls-roots] default build → reqwest must use webpki roots, not native"
def="$(reqwest_feature_line default)"
[ -n "$def" ] || fail "could not determine reqwest features for the default build"
case ",$def," in
    *,rustls-tls-webpki-roots,*) : ;;
    *) fail "default build is missing reqwest/rustls-tls-webpki-roots (got: $def)" ;;
esac
case "$def" in
    *rustls-tls-native-roots*)
        fail "default build pulls reqwest native roots — re-introduces the cold-start native-cert scan (got: $def)" ;;
esac

echo "[tls-roots] fips build → reqwest must use native roots, not webpki"
fips="$(reqwest_feature_line fips)"
[ -n "$fips" ] || fail "could not determine reqwest features for the fips build"
case "$fips" in
    *rustls-tls-native-roots*) : ;;
    *) fail "fips build is missing reqwest native roots (got: $fips)" ;;
esac
case ",$fips," in
    *,rustls-tls-webpki-roots,*)
        fail "webpki roots cross-linked into the fips build (got: $fips)" ;;
esac

echo "[tls-roots] OK — reqwest root sources are correct for both builds"
