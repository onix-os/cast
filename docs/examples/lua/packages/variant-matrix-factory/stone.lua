-- Two closed axes form one exhaustive configuration value. Unselected matrix
-- cells never contribute dependencies to the frozen closure.
local factory = cast.import("package.lua")
local matrix = cast.import("matrix.lua")

return factory.make(matrix.postgresql, matrix.opentelemetry)
