
project(
  'cast-meson-fixture',
  'c',
  version: '1.0.0',
  default_options: ['warning_level=3', 'werror=true'],
)

zlib = dependency('zlib', method: 'pkg-config', required: true)

fixture = executable(
  'cast-meson-fixture',
  'hello.c',
  dependencies: zlib,
  install: true,
)

check_file_input = executable(
  'cast-meson-check-file-input',
  'check-file-input.c',
  install: false,
)

test('cast-meson-zlib-round-trip', fixture)
test(
  'cast-meson-declared-file-check-input',
  check_file_input,
  args: [fixture],
)
