local volfdnr = ARGV[1]
local message = ARGV[2]
local chat_ids = {} 

-- Read chats ids from ARGV[3] onwards
for i = 3, #ARGV do
    table.insert(chat_ids, ARGV[i])
end

-- Add volfdnr to known items
if redis.call("SADD", KEYS[1], volfdnr) == 0 then
    return 0  -- Abort if item was already processed
end

-- Add messages to the queue 
for _, chat_id in ipairs(chat_ids) do
    redis.call("RPUSH", KEYS[2], chat_id .. ":" .. message)
end

return 1
