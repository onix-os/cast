-- The root chooses one complete runtime environment. Changing a provider or
-- path changes the evaluated package and frozen derivation together.
local runtime = cast.import("runtime.lua")
local factory = cast.import("package.lua")

return factory.make(runtime)
