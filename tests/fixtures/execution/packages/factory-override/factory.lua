-- A Stone-native package factory: callers inject typed package values and the
-- factory returns the immutable package the caller then owns.
return function(inputs)
    return {
        meta = inputs.meta,
        builder = inputs.builder,
        sources = inputs.sources,
        architectures = inputs.architectures,
    }
end
