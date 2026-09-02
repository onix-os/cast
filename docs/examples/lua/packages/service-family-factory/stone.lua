-- One release record feeds every member, while this declaration selects only
-- the daemon closure and its service/configuration output contract.
local family = cast.import("family.lua")
local factory = cast.import("package.lua")
local release = cast.import("release.lua")

return factory.make(release, family.daemon)
