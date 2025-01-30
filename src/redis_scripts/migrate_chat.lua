local old_value = redis.call('HGET', KEYS[1], ARGV[1]);
if old_value then
    redis.call('HSET', KEYS[1], ARGV[2], old_value);
    redis.call('HDEL', KEYS[1], ARGV[1]); return 1;
else
    return 0;
end
