-- The package factory receives a closed, typed source graph. Linking the
-- independently locked trees into the expected build layout is also explicit.
local factory = cast.import("package.lua")
local repositories = cast.import("repositories.lua")

return factory.make(repositories)
