#!/bin/sh
# Restores Redis and the signaling volume from a snapshot made by backup.sh.
# Run from the deploy directory on the server:
#
#   docker compose -f docker-compose.yml -f docker-compose.prod.yml exec backup ls /backups
#   deploy/restore.sh 20261004T030000Z
#
# Clients keep their sessions: Redis holds only prekeys, links and offline
# inboxes, so messages queued after the snapshot are lost and links created
# since then stop resolving.
set -eu

stamp="${1:?usage: deploy/restore.sh <stamp>, e.g. 20261004T030000Z}"
compose() { docker compose -f docker-compose.yml -f docker-compose.prod.yml "$@"; }
# Volumes are named <project>_<volume>; compose names the project after
# this directory unless COMPOSE_PROJECT_NAME says otherwise.
project="${COMPOSE_PROJECT_NAME:-$(basename "$PWD")}"
redis_image="$(compose config --images | grep -m 1 '^redis:')"
volume() { printf '%s_%s' "$project" "$1"; }

for file in "redis-$stamp.rdb" "signaling-$stamp.tgz"; do
    docker run --rm -v "$(volume backups):/b:ro" --entrypoint test "$redis_image" -s "/b/$file" \
        || { echo "no backup /backups/$file" >&2; exit 1; }
done

echo "Stopping signaling and Redis"
compose stop signaling redis

docker run --rm -v "$(volume redis-data):/data" -v "$(volume backups):/b:ro" --entrypoint sh "$redis_image" -c \
    "rm -rf /data/appendonlydir /data/dump.rdb && cp /b/redis-$stamp.rdb /data/dump.rdb && chown 999:999 /data/dump.rdb"
docker run --rm -v "$(volume signaling-data):/data" -v "$(volume backups):/b:ro" --entrypoint sh "$redis_image" -c \
    "find /data -mindepth 1 -delete && tar -C /data -xzf /b/signaling-$stamp.tgz && chown -R 10001:10001 /data"

# With appendonly on and no AOF, Redis would start empty and ignore the
# snapshot. Load it with AOF off, then let Redis write a fresh AOF from it.
echo "Loading the Redis snapshot"
docker run -d --name cypher-restore --network none -v "$(volume redis-data):/data" "$redis_image" \
    redis-server --appendonly no > /dev/null
trap 'docker rm -f cypher-restore > /dev/null 2>&1 || true' EXIT
until docker exec cypher-restore redis-cli ping > /dev/null 2>&1; do sleep 1; done
docker exec cypher-restore redis-cli config set appendonly yes > /dev/null
until docker exec cypher-restore redis-cli info persistence | grep -q 'aof_rewrite_in_progress:0' \
    && docker exec cypher-restore redis-cli info persistence | grep -q 'aof_enabled:1'; do
    sleep 1
done
echo "Restored $(docker exec cypher-restore redis-cli dbsize) Redis keys"
docker rm -f cypher-restore > /dev/null

compose up -d --wait redis signaling
echo "Restored $stamp"
