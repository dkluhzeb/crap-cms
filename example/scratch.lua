---@class (exact) Foo
---@field a number

---@type Foo
local x = { a = 1, b = 2 } -- should flag `b`

x.c = 3 -- should also flag `c`
