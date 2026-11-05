
    local stored = redis.call('GET', KEYS[2])
    if stored and stored >= ARGV[1] then return 0 end
    redis.call('SET', KEYS[1], ARGV[2], 'EX', ARGV[3])
    redis.call('SET', KEYS[2], ARGV[1], 'EX', ARGV[3])
    return 1