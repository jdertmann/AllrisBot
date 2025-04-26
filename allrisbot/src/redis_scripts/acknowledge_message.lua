-- KEYS[1] = register_chat_key(chat_id)
-- KEYS[2] = stream name
-- ARGV[1] = candidate next ID

local last_sent = redis.call('HGET', KEYS[1], "last_sent")
local stream = KEYS[2]
local candidate_id = ARGV[1]

-- If no previous ID is stored, return 0
if not last_sent then
    return 0
end

-- Read the next entry after last_sent
local res = redis.call('XREAD', "COUNT", 1,  "STREAMS", stream, last_sent)

-- Check if the candidate_id is the immediate next entry
if res and res[1][2][1][1] == candidate_id then
    -- Update the key to the new ID
    redis.call('HSET', KEYS[1], "last_sent", candidate_id)
    return 1
end

return 0
