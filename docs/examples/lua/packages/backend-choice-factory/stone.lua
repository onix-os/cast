-- A closed variant makes the three backends mutually exclusive. Adding a
-- backend requires extending the factory instead of coordinating several
-- independent booleans.
local factory = cast.import("package.lua")

return factory.make("libressl")
