-- The root composes two independent source-lock records with one pure package
-- factory; neither application nor vendor identity is inferred from the other.
local factory = cast.import("package.lua")
local sources = cast.import("sources.lua")

return factory.make(sources)
