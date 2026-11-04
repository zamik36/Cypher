
    local now, opening = tonumber(ARGV[8]), tonumber(ARGV[9])
    local opened = redis.call('EXPIRETIME', KEYS[3])
    if ARGV[6] ~= '1' and opened < 0
      and budget_held(5) + opening > tonumber(ARGV[7]) then
      return false
    end
    budget_take(5, now, opened, opening)
    if ARGV[6] == '1' then
      redis.call('DEL', KEYS[3], KEYS[4])
    else
      redis.call('SET', KEYS[3], 1, 'EX', ARGV[5])
      budget_add(5, now, redis.call('EXPIRETIME', KEYS[3]), opening)
    end
    if redis.call('EXISTS', KEYS[2]) == 1 then return {} end
    local items = redis.call('LRANGE', KEYS[1], 0, tonumber(ARGV[2]) - 1)
    local out, total = {}, 0
    for i, v in ipairs(items) do
      if total + #v > tonumber(ARGV[3]) and i > 1 then break end
      total = total + #v
      out[#out + 1] = v
    end
    if #out > 0 then
      local claim = ARGV[1] .. ':' .. #out .. ':' .. total .. ':'
        .. redis.call('EXPIRETIME', KEYS[1])
      redis.call('SET', KEYS[2], claim, 'EX', ARGV[4])
    end
    return out