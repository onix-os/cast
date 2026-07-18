{
  description = "Cast development shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs?rev=4c1018dae018162ec878d42fec712642d214fdfa";
    rust-overlay.url = "github:oxalica/rust-overlay?rev=3c27f4c92a7d977556dd2c10bb564d9c61b375e9";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.clang
            pkgs.cmake
            (pkgs.diesel-cli.override {
              sqliteSupport = true;
              postgresqlSupport = false;
              mysqlSupport = false;
            })
            pkgs.git
            pkgs.gcc
            pkgs.go
            pkgs.gzip
            pkgs.gnumake
            pkgs.jq
            pkgs.pkg-config
            pkgs.typos
            pkgs.valgrind
            pkgs.xz
            pkgs.zstd
          ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.appstream
            pkgs.dash
            pkgs.desktop-file-utils
            pkgs.fontconfig
            pkgs.gettext
            pkgs.glib
            pkgs.glibcLocales
            pkgs.libxml2
            pkgs.shared-mime-info
            pkgs.systemd
          ];

          CC = "${pkgs.clang}/bin/clang";
          CXX = "${pkgs.clang}/bin/clang++";
          LOCALE_ARCHIVE = pkgs.lib.optionalString pkgs.stdenv.isLinux "${pkgs.glibcLocales}/lib/locale/locale-archive";
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      }
    );
}
