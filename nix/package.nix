{
  lib,
  rustPlatform,
  dependencies,
  makeWrapper,
  makeDesktopItem,
  sevenzip,
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
    "-p"
    "smartzip-archive"
  ];
  strictDeps = true;
  nativeBuildInputs = dependencies.nativeBuildInputs ++ [ makeWrapper ];
  buildInputs = dependencies.buildInputs;
  # The package smoke check exercises the installed wrappers and real backend.
  # Workspace tests remain available through `nix develop -c cargo test --workspace --locked`.
  doCheck = false;
  postInstall = ''
    mkdir -p "$out/share/licenses"
    install -Dm755 ${sevenzip}/lib/smartzip/7z.so "$out/lib/smartzip/7z.so"
    cp -r ${sevenzip}/share/licenses/7zip "$out/share/licenses/7zip"
    mkdir -p "$out/share/smartzip"
    cp ${sevenzip}/share/smartzip/7zip-source.tar.xz "$out/share/smartzip/"
    install -Dm644 ${../resources/fonts/OFL.txt} "$out/share/licenses/smartzip/NotoSansCJK-OFL.txt"
    install -Dm644 ${desktop}/share/applications/*.desktop \
      "$out/share/applications/org.smartzip.SmartZip.desktop"
  '';
  postFixup = ''
    wrapProgram "$out/bin/smartzip" \
      --set SMARTZIP_7Z_LIBRARY "$out/lib/smartzip/7z.so" \
      --set SMARTZIP_ARCHIVE_WORKER "$out/bin/smartzip-archive-worker"
    wrapProgram "$out/bin/smartzip-gui" \
      --set SMARTZIP_7Z_LIBRARY "$out/lib/smartzip/7z.so" \
      --set SMARTZIP_ARCHIVE_WORKER "$out/bin/smartzip-archive-worker" \
      --set SMARTZIP_GUI_LAUNCHER "$out/bin/smartzip-gui" \
      --suffix PATH : ${
        lib.makeBinPath [
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
