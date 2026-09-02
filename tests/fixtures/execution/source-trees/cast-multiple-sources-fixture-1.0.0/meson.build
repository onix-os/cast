project(
  'cast-multiple-sources-fixture',
  'c',
  version: '1.0.0',
  default_options: ['c_std=c11', 'warning_level=3', 'werror=true'],
)

vendor_protocol = include_directories('../vendor-protocol/include')

fixture = executable(
  'cast-multiple-sources-fixture',
  'src/main.c',
  include_directories: vendor_protocol,
  install: true,
)

test(
  'cast-multiple-sources-fixture-runs',
  fixture,
  args: ['--self-test'],
  protocol: 'exitcode',
)
