local broadcasts_key = KEYS[1]
local known_volfdnrs_key = KEYS[2]
local volfdnr = ARGV[1]
local message = ARGV[2]

-- Add volfdnr to known items
if redis.call("SADD", known_volfdnrs_key, volfdnr) == 0 then
    return nil  -- Abort if item was already processed
end

return redis.call("XADD", broadcasts_key, "*", "message", message, "volfdnr", volfdnr)
