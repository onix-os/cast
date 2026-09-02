-- The scope is ordinary authored data. Factories receive capabilities
-- explicitly; field names are not inferred from function arguments.
return {
    compression = { kind = "pkg_config", value = "zlib" },
    crypto = { kind = "pkg_config", value = "libressl" },
    database = { kind = "pkg_config", value = "sqlite3" },
    documentation = { kind = "binary", value = "doxygen" },
}
