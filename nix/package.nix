{
  lib,
  rustPlatform,
  dependencies,
  makeWrapper,
  makeDesktopItem,
  _7zz,
  xdg-utils,
  desktop-file-utils,
}:
let
  root = ../.;
  archiveTypes = builtins.fromJSON (builtins.readFile ../resources/file-types.json);
  desktop = makeDesktopItem {
    name = "org.smartzip.SmartZip";
    desktopName = "SmartZip";
    comment = "Extract and browse archives";
    exec = "smartzip-gui -- %F";
    terminal = false;
    categories = [
      "Utility"
      "Archiving"
    ];
    mimeTypes = map (format: format.mime) archiveTypes;
    actions.QuickExtract = {
      name = "Quick extract with SmartZip";
      exec = "smartzip-gui --quick-extract -- %F";
    };
  };
in
rustPlatform.buildRustPackage {
  pname = "smartzip";
  version = (builtins.fromTOML (builtins.readFile ../crates/smartzip-cli/Cargo.toml)).package.version;
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../rust-toolchain.toml
      ../crates
      ../resources
      ../LICENSE
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [
    "-p"
    "smartzip-cli"
    "-p"
    "smartzip-gui"
  ];
  strictDeps = true;
  nativeBuildInputs = dependencies.nativeBuildInputs ++ [ makeWrapper ];
  buildInputs = dependencies.buildInputs;
  # The package smoke check exercises the installed wrappers and real backend.
  # Workspace tests remain available through `nix develop -c cargo test --workspace --locked`.
  doCheck = false;
  postInstall = ''
    install -Dm644 ${desktop}/share/applications/*.desktop \
      "$out/share/applications/org.smartzip.SmartZip.desktop"
  '';
  postFixup = ''
    wrapProgram "$out/bin/smartzip" \
      --suffix PATH : ${lib.makeBinPath [ _7zz ]}
    wrapProgram "$out/bin/smartzip-gui" \
      --set SMARTZIP_GUI_LAUNCHER "$out/bin/smartzip-gui" \
      --suffix PATH : ${
        lib.makeBinPath [
          _7zz
          xdg-utils
          desktop-file-utils
        ]
      } \
      --prefix LD_LIBRARY_PATH : "/run/opengl-driver/lib:${dependencies.libraryPath}"
  '';
  meta = {
    description = "Archive detection, extraction and browsing with CLI and GUI";
    homepage = "https://github.com/name8102/SmartZip";
    license = lib.licenses.mit;
    mainProgram = "smartzip";
    platforms = lib.platforms.linux;
  };
}
