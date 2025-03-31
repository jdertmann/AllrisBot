-- KEYS[1] = REGISTERED_CHATS_KEY
-- KEYS[2] = broadcast_last_message_key(old_chat_id)
-- KEYS[3] = broadcast_last_message_key(new_chat_id)
-- ARGV[1] = old_chat_id
-- ARGV[2] = new_chat_id

local function max_stream_id(id1, id2)
    if not id1 then
        if id2 then
            return id2
        else 
            return "0-0"
        end
    elseif not id2 then
        return id1
    end

    local t1, s1 = string.match(id1, '^(%d+)%-(%d+)$')
    local t2, s2 = string.match(id2, '^(%d+)%-(%d+)$')
  
    t1, s1 = tonumber(t1), tonumber(s1)
    t2, s2 = tonumber(t2), tonumber(s2)
  
    if t1 < t2 then
        return id2
    elseif t1 > t2 then
        return id1
    elseif s1 < s2 then
        return id2
    else
        return id1
    end
end

local old_chat_removed = redis.call("SREM", KEYS[1], ARGV[1])

if old_chat_removed < 1 then
    return
end

local old_last_sent = redis.call("HGET", KEYS[2], "last_sent")
local old_filter = redis.call("HGET", KEYS[2], "filter")
local new_last_sent = redis.call("HGET", KEYS[3], "last_sent")

redis.call("SADD", KEYS[1], ARGV[2])
redis.call("DEL", KEYS[2])
redis.call("HSET", KEYS[2], "migrated", ARGV[2])
redis.call("EXPIRE", KEYS[2], 36000)
redis.call("HSET", KEYS[3], "filter", old_filter, "last_sent", max_stream_id(old_last_sent, new_last_sent))
