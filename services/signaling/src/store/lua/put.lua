
    if redis.call('EXISTS', KEYS[3]) == 0 then return -1 end
    local size = #ARGV[1]
    local bytes = tonumber(redis.call('GET', KEYS[2]) or '0')
    if redis.call('LLEN', KEYS[1]) >= tonumber(ARGV[2])
      or bytes + size > tonumber(ARGV[3]) then return 0 end
    local cost = size + tonumber(ARGV[7])
    if budget_held(4) + cost > tonumber(ARGV[5]) then return -2 end
    redis.call('RPUSH', KEYS[1], ARGV[1])
    redis.call('EXPIRE', KEYS[1], ARGV[4], 'NX')
    redis.call('INCRBY', KEYS[2], size)
    redis.call('EXPIRE', KEYS[2], ARGV[4], 'NX')
    budget_add(4, tonumber(ARGV[6]), redis.call('EXPIRETIME', KEYS[1]), cost)
    return 1