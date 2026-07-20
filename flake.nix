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
        python =
          assert pkgs.python314.version == "3.14.3";
          pkgs.python314;
        pythonPackaging =
          assert pkgs.python314Packages.packaging.version == "25.0";
          pkgs.python314Packages.packaging;
        pythonSetuptools = pkgs.python314Packages.setuptools.overridePythonAttrs (_: rec {
          pname = "setuptools";
          version = "82.0.1";
          src = pkgs.fetchPypi {
            inherit pname version;
            hash = "sha256-fYcmgsXQHP3gfae8zHtlRp09yiAzGFFa2h3l7aNe+/k=";
          };
        });
        pythonWheel = pkgs.python314Packages.wheel.overridePythonAttrs (old: rec {
          pname = "wheel";
          version = "0.47.0";
          src = pkgs.fetchPypi {
            inherit pname version;
            hash = "sha256-zHK9EAm6DPY5IuKPlNnYO5IKorso95ijHQaRsC+jybM=";
          };
          dependencies = (old.dependencies or [ ]) ++ [ pythonPackaging ];
        });
        pythonTypingExtensions =
          assert pkgs.python314Packages.typing-extensions.version == "4.15.0";
          pkgs.python314Packages.typing-extensions;
        pythonToolchain = python.withPackages (_: [
          pythonSetuptools
          pythonTypingExtensions
          pythonWheel
        ]);
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.bashNonInteractive
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
            pkgs.ninja
            pkgs.pkg-config
            pythonToolchain
            pkgs.ripgrep
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
            pkgs.util-linux
          ];

          CC = "${pkgs.clang}/bin/clang";
          CXX = "${pkgs.clang}/bin/clang++";
          LOCALE_ARCHIVE = pkgs.lib.optionalString pkgs.stdenv.isLinux "${pkgs.glibcLocales}/lib/locale/locale-archive";
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      }
    );
}
