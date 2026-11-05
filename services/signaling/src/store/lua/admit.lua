
                local n = redis.call('INCR', KEYS[1])
                if n == 1 then redis.call('EXPIRE', KEYS[1], ARGV[2]) end
                if n > tonumber(ARGV[1]) then return 0 end
                return 1