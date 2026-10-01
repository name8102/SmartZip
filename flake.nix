{
  description = "SmartZip CLI, desktop application and development environment";

  inputs = {
    nixpkgs.url = "https://channels.nixos.org/nixos-26.05/nixexprs.tar.xz";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      projectFor =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          rustToolchain = pkgs.rust-bin.stable."1.98.1".minimal.override {
            extensions = [
              "clippy"
              "rustfmt"
              "rust-src"
              "rust-analyzer"
            ];
          };
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          dependencies = import ./nix/dependencies.nix { inherit pkgs; };
          smartzip = pkgs.callPackage ./nix/package.nix { inherit dependencies rustPlatform; };
        in
        {
          inherit
            pkgs
            dependencies
            smartzip
            rustToolchain
            ;
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          project = projectFor system;
        in
        {
          default = project.smartzip;
          smartzip = project.smartzip;
        }
      );

      apps = forAllSystems (
        system:
        let
          package = self.packages.${system}.smartzip;
          app = executable: {
            type = "app";
            program = "${package}/bin/${executable}";
            meta.description = "Run ${executable}";
          };
        in
        {
          default = app "smartzip";
          smartzip = app "smartzip";
          smartzip-gui = app "smartzip-gui";
        }
      );

      devShells = forAllSystems (
        system:
        let
          inherit (projectFor system) pkgs dependencies rustToolchain;
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = dependencies.nativeBuildInputs;
            buildInputs = dependencies.buildInputs;
            packages = with pkgs; [
              rustToolchain
              just
              python314
              git
              ripgrep
              _7zz
              zip
              unzip
            ];
            # GPUI loads graphics libraries at runtime; GPU drivers belong to the host.
            LD_LIBRARY_PATH = dependencies.libraryPath;
            shellHook = ''
              export LD_LIBRARY_PATH="/run/opengl-driver/lib:$LD_LIBRARY_PATH"
              export CARGO_TARGET_DIR="''${CARGO_TARGET_DIR:-$PWD/target/nix}"
            '';
          };
        }
      );

      checks = forAllSystems (
        system:
        let
          inherit (projectFor system) pkgs smartzip;
        in
        {
          smoke =
            pkgs.runCommand "smartzip-nix-smoke"
              {
                nativeBuildInputs = [
                  smartzip
                  pkgs._7zz
                ];
              }
              ''
                export HOME="$TMPDIR/home"
                export XDG_CONFIG_HOME="$HOME/config"
                export XDG_DATA_HOME="$HOME/data"
                export XDG_CACHE_HOME="$HOME/cache"
                mkdir -p "$HOME" "$TMPDIR/input"
                printf 'SmartZip Nix smoke test\n' > "$TMPDIR/input/payload.txt"
                7zz a "$TMPDIR/archive.zip" "$TMPDIR/input/payload.txt" > /dev/null
                smartzip --version
                smartzip doctor --json > "$TMPDIR/doctor.json"
                smartzip list "$TMPDIR/archive.zip" --json > "$TMPDIR/list.json"
                smartzip test "$TMPDIR/archive.zip" --json > "$TMPDIR/test.json"
                smartzip extract "$TMPDIR/archive.zip" --output "$TMPDIR/output" \
                  --non-interactive --on-conflict rename --suspicious-encoding accept --json \
                  > "$TMPDIR/extract.json"
                cmp "$TMPDIR/input/payload.txt" "$(find "$TMPDIR/output" -name payload.txt -type f)"
                mkdir -p "$out"
                cp "$TMPDIR/"{doctor,list,test,extract}.json "$out/"
              '';
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
