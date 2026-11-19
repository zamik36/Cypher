
    local evicted = 0
    if not redis.call('ZSCORE', KEYS[1], ARGV[1])
      and redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[2]) then
      evicted = tonumber(redis.call('ZPOPMIN', KEYS[1])[1])
    end
    local latest = redis.call('ZRANGE', KEYS[1], -1, -1, 'WITHSCORES')[2]
    redis.call('ZADD', KEYS[1], tonumber(latest or '0') + 1, ARGV[1])
    redis.call('EXPIRE', KEYS[1], ARGV[3])
    return evicted