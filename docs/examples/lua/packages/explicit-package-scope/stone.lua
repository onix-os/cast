-- Selection is explicit and returns one package. The unselected server
-- factory contributes no dependencies or sources to the client's closure.
local packages = cast.import("packages.lua")
local scope = cast.import("scope.lua")

return packages.client(scope)
