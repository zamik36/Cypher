#!/bin/sh
# Daily snapshot of Redis and of the signaling volume (its onion key) into
# /backups, keeping the last $KEEP_DAYS days. The time of the last success
# goes to node-exporter's textfile collector for the BackupStale alert.
# Restore with deploy/restore.sh.
set -u

KEEP_DAYS=14

backup() {
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    redis-cli -h redis --rdb "/backups/redis-$stamp.rdb.tmp" > /dev/null \
        && tar -C /signaling -czf "/backups/signaling-$stamp.tgz.tmp" . \
        && mv "/backups/redis-$stamp.rdb.tmp" "/backups/redis-$stamp.rdb" \
        && mv "/backups/signaling-$stamp.tgz.tmp" "/backups/signaling-$stamp.tgz"
}

while :; do
    if backup; then
        echo "backup $stamp done"
        printf 'cypher_backup_last_success_timestamp_seconds %s\n' "$(date +%s)" > /metrics/backup.prom.tmp
        chmod 0644 /metrics/backup.prom.tmp
        mv /metrics/backup.prom.tmp /metrics/backup.prom
    else
        echo "backup $stamp failed" >&2
        rm -f /backups/*.tmp
    fi
    find /backups -type f \( -name 'redis-*.rdb' -o -name 'signaling-*.tgz' \) -mtime +"$KEEP_DAYS" -delete
    sleep 86400
done
