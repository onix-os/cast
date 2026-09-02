AC_INIT([cast-autotools-fixture], [1.0.0], [fixtures@invalid.example])
AC_CONFIG_SRCDIR([hello.c])
AC_CONFIG_HEADERS([config.h])
AM_INIT_AUTOMAKE([foreign])
AC_PROG_CC

AS_IF([test -z "$build_alias"], [AC_MSG_ERROR([--build is required])])
AS_IF([test -z "$host_alias"], [AC_MSG_ERROR([--host is required])])
AC_DEFINE_UNQUOTED(
    [CAST_AUTOTOOLS_BUILD_ALIAS],
    ["$build_alias"],
    [Build platform supplied by the frozen repository policy.]
)
AC_DEFINE_UNQUOTED(
    [CAST_AUTOTOOLS_HOST_ALIAS],
    ["$host_alias"],
    [Host platform supplied by the frozen repository policy.]
)

AC_CONFIG_FILES([Makefile])
AC_OUTPUT
