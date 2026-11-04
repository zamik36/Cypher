
    -- The counter for what expires at `expires` (unix seconds, as
    -- EXPIRETIME gives), and when that counter expires.
    local function budget_bucket(first, now, expires)
      local today = math.floor(now / 86400)
      local day = math.floor(expires / 86400) - today
      day = math.max(0, math.min(day, #KEYS - first))
      return KEYS[first + day], (today + day + 1) * 86400
    end
    local function budget_held(first)
      local held = 0
      for _, v in ipairs(redis.call('MGET', unpack(KEYS, first))) do
        held = held + math.max(tonumber(v or '0'), 0)
      end
      return held
    end
    local function budget_add(first, now, expires, cost)
      local bucket, ends = budget_bucket(first, now, expires)
      redis.call('INCRBY', bucket, cost)
      redis.call('EXPIREAT', bucket, ends)
    end
    -- Only a counter that still lives is lowered: one that expired counts
    -- nothing any more.
    local function budget_take(first, now, expires, cost)
      if expires < 0 then return end
      local bucket = budget_bucket(first, now, expires)
      if redis.call('EXISTS', bucket) == 1
        and redis.call('DECRBY', bucket, cost) <= 0 then
        redis.call('DEL', bucket)
      end
    end
