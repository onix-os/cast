-- The root selects an explicit interpreter set. Changing either target
-- changes its build/check programs, installed shebang, runtime closure, and
-- frozen derivation instead of consulting ambient interpreter discovery.
local interpreters = cast.import("interpreters.lua")
local factory = cast.import("package.lua")

return factory.make(interpreters)
