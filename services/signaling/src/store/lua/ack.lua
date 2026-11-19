
    local v = redis.call('GET', KEYS[2])
    if not v or string.sub(v, 1, 32) ~= ARGV[1] then return 0 end
    redis.call('DEL', KEYS[2])
    local n, bytes, expires =
      string.match(string.sub(v, 34), '^(%d+):(%d+):(%-?%d+)$')
    n, bytes, expires = tonumber(n), tonumber(bytes), tonumber(expires)
    if not expires or redis.call('EXPIRETIME', KEYS[1]) ~= expires then
      return 0
    end
    redis.call('LTRIM', KEYS[1], n, -1)
    if redis.call('DECRBY', KEYS[3], bytes) <= 0 then
      redis.call('DEL', KEYS[3])
    end
    budget_take(4, tonumber(ARGV[2]), expires, bytes + n * tonumber(ARGV[3]))
    return n