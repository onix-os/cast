-- One typed choice changes the source, tool, setup, and output graphs as one
-- coherent value instead of leaving independently editable switches.
local factory = cast.import("package.lua")
local features = cast.import("features.lua")

return factory.make(features)
