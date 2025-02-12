local value = redis.call('HGET', KEYS[1], ARGV[1])
if value ~= nil then
    redis.call('HDEL', KEYS[1], ARGV[1])
end
return value