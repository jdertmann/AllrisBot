-- KEYS[1] = SCHEDULED_MESSAGES_KEY
-- KEYS[2] = REGISTERED_CHATS_KEY
-- KEYS[3] = register_chat_key(chat_id)
-- ARGV[1] = chat_id
-- ARGV[2] = filter

local entries = redis.call("XREVRANGE", KEYS[1], "+", "-", "COUNT", 1)
local last_entry_id = "0-0"

if entries and #entries > 0 then
    last_entry_id = entries[1][1]
end

local hset_result = redis.call("SADD", KEYS[2], ARGV[1])
redis.call("HSET", KEYS[3], "filter", ARGV[2])
redis.call("HSETNX", KEYS[3], "last_sent", last_entry_id)

return hset_result
