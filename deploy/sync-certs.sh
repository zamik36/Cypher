#!/bin/sh
# Copies the certificate Caddy obtained for $DOMAIN into /certs, readable only
# by the user gateway and relay run as. They never see the rest of Caddy's
# data, its ACME account key included. Re-checks every minute; the services
# pick up a renewal by themselves.
set -eu

src="/caddy-data/caddy/certificates/acme-v02.api.letsencrypt.org-directory/${DOMAIN:?}"
owner=10001

# copy <from> <to>: replaces <to> atomically when <from> is newer. Compared by
# time, as the copy belongs to the services' user and root cannot read it.
copy() {
    [ -f "$1" ] || return 0
    if [ -e "$2" ] && ! [ "$1" -nt "$2" ]; then return 0; fi
    rm -f "$2.tmp"
    cp "$1" "$2.tmp"
    # Mode first: once the file is handed over, root no longer owns it and,
    # without CAP_FOWNER, could not change it.
    chmod 0400 "$2.tmp"
    chown "$owner:$owner" "$2.tmp"
    mv "$2.tmp" "$2"
    echo "updated $2"
}

while :; do
    copy "$src/$DOMAIN.key" /certs/key.pem
    copy "$src/$DOMAIN.crt" /certs/cert.pem
    sleep 60
done
