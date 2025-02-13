local volfdnr = ARGV[1]
local message = ARGV[2]
local fields = {} 

-- Read fields from ARGV[4] onwards
for i = 3, #ARGV do
    table.insert(fields, ARGV[i])
end

-- Add volfdnr to known items
if redis.call("SADD", KEYS[1], volfdnr) == 0 then
    return 0  -- Abort if item was already processed
end

-- Add messages to the queue 
for _, field in ipairs(fields) do
    redis.call("HSET", KEYS[2], field, message)
end

return 1
