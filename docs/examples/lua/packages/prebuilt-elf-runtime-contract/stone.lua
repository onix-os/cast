-- The root makes one target-specific artifact choice explicitly.
local artifact = cast.import("artifact.lua")
local factory = cast.import("package.lua")

return factory.make(artifact)
