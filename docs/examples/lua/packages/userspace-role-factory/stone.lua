-- NixOS-style system roles are the inspiration, but selection is an ordinary
-- typed function call rather than implicit module merging.
local roles = cast.import("roles.lua")
local factory = cast.import("package.lua")

return factory.make(roles.server)
