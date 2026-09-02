local scope = cast.import("scope.lua")
local factory = cast.import("package.lua")

local packages = scope.with_observability(scope.base)

return factory.make(packages)
