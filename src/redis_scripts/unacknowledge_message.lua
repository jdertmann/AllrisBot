-- KEYS[1] = key holding the last seen ID
-- KEYS[2] = stream name
-- ARGV[1] = id to mark unseen

local key = KEYS[1]
local stream = KEYS[2]
local expected_current_id = ARGV[1]

-- Get the previous stream entry before expected_current_id
local res = redis.call('XREVRANGE', stream, expected_current_id, '-', 'COUNT', 2)

local prev_id = "0-0"  

if res and #res > 1 then
    prev_id = res[2][1]
end

local current_id = redis.call('HGET', key, "last_sent")

if current_id == prev_id then
    -- makes this script idempotent
    return 1
elseif current_id == expected_current_id then
    redis.call('HSET', key, "last_sent", prev_id)
    return 1
else
    return 0
end
